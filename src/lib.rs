use std::path::Path;

use base64::prelude::*;
use gtcorpusutil::KorpMonoFilePath;
use hfst::{HfstInputStream, HfstInputStreamError, HfstTransducer};
use regex::Regex;
use without_ats::without_ats_iter;

const REGEX: &'static str = r#"\[\[\[GEN:#[^\]]+]]]"#;
// TODO could maybe just capture the things directly, without having to
// do the split in Processor::split_gen()
//const REGEX: &'static str = r#"[[[GEN:(?<gen>\w+):::(?<reading>[^\]]+)]]]"#;

pub struct Processor {
    re: Regex,
    fst: HfstTransducer,
}

pub struct GenAttempt {
    /// Input sent to generator
    pub input: String,

    /// Results of generation. If non-empty, the first result is used as
    /// the final replacement in the file. If empty, there may be additional
    /// attempts made, in the upper `attempts`.
    pub result: Vec<String>,
}

impl GenAttempt {
    pub fn is_success(&self) -> bool {
        !self.result.is_empty()
    }
}

pub struct GenStatus {
    /// The word form of the word we're generating the lemma for
    pub word_form: String,

    /// The original reading
    pub reading: String,

    /// All the attempts made at generating a lemma. The first attempt with
    /// a non-empty result, will have the first of those results be used as
    /// the final replacement in the file.
    pub attempts: Vec<GenAttempt>,
}

impl GenStatus {
    fn new(word_form: String, reading: String) -> Self {
        Self {
            word_form,
            reading,
            attempts: vec![],
        }
    }

    pub fn is_success(&self) -> bool {
        self.attempts.iter().any(|attempt| attempt.is_success())
    }
}

impl Processor {
    pub fn new<P: AsRef<Path>>(fst_path: P) -> Result<Processor, LoadFstError> {
        Ok(Processor {
            re: Regex::new(REGEX).unwrap(),
            fst: load_fst(fst_path)?,
        })
    }

    /// Helper function to split up the
    /// `[[[GEN:<generation_string>:::<base64-encoded original reading>]]]`
    /// string we search for, into the generation string, and the decoded
    /// reading
    fn split_gen(s: &str) -> Result<(&str, String), ProcessFileError> {
        let (gen_str, reading) = s
            .strip_prefix("[[[GEN:#")
            .ok_or(ProcessFileError::RegexMatchError)?
            .strip_suffix("]]]")
            .ok_or(ProcessFileError::RegexMatchError)?
            .split_once(":::")
            .ok_or(ProcessFileError::MissingTripleColon)?;
        let reading = BASE64_STANDARD.decode(reading)?;
        Ok((gen_str, String::from_utf8(reading)?))
    }

    pub fn process(
        &self,
        file: &KorpMonoFilePath,
    ) -> Result<(String, Vec<GenStatus>), ProcessFileError> {
        let fst = &self.fst;
        let s = file.read_to_string()?;
        let mut new_s = String::with_capacity(s.len());
        let mut statuses = vec![];

        fn do_lookup(
            s: &str,
            new_s: &mut String,
            fst: &HfstTransducer,
            status: &mut GenStatus,
        ) -> bool {
            let result: Vec<_> = fst
                .lookup(s)
                .into_iter()
                .map(|(result, _weight)| result)
                .collect();

            let found = if let Some(first) = result.first() {
                new_s.extend(without_ats_iter(first));
                true
            } else {
                false
            };

            status.attempts.push(GenAttempt {
                input: s.to_owned(),
                result,
            });

            found
        }

        // note: not using regex::replace_all(), because we want to return early
        // on errors

        let mut i = 0;

        for m in self.re.find_iter(&s) {
            // find the word form, used only for debugging
            let word_form = find_word_form(&s, m.start()).to_string();

            let before_match = &s[i..m.start()];
            new_s.push_str(before_match);

            let (gen_str, reading) = Self::split_gen(m.as_str())?;
            statuses.push(GenStatus::new(word_form.clone(), reading));
            let status = statuses.last_mut().unwrap();
            println!("{gen_str}");

            // For each of the strategies of generation, first check if
            // it applies (the first if-let), i.e. if that strategy makes
            // sense to do for this input-string, e.g. a strategy that only
            // changes something of adverbs would return None for a starting
            // generation string of a verb.
            // Then, if the strategy applies, try generating the lemma with
            // this updated input generation string. If that succeeds, we
            // can move on to the next GEN (the continue). If it doesn't,
            // we try the next strategy.
            let mut found_lemma = false;
            for strategy in GENERATION_STRATEGIES {
                if let Some(updated_input) = strategy(gen_str) {
                    found_lemma = do_lookup(&updated_input, &mut new_s, &fst, status);
                    if found_lemma {
                        break;
                    }
                }
            }

            if !found_lemma {
                // no strategies were able to generate a lemma. Use the word form
                // as lemma? Alternative: If a compound, use the lemma of the
                // last part as lemma.
                new_s.push_str("=====FALLBACK====");
                new_s.push_str(&word_form.trim());
            }

            // update `i` to point at the next index after the match, so that
            // on the next iteration, everything between the two matches gets
            // pushed correctly.
            i = m.end();
        }

        // remember to append what's left after all the matches
        new_s.push_str(&s[i..]);

        Ok((new_s, statuses))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProcessFileError {
    #[error("Io error when reading file: {0}")]
    Io(#[from] std::io::Error),

    #[error(
        "Internal regex error (this should never happen unless there is a bug in the regex engine!)"
    )]
    RegexMatchError,

    #[error(
        "Internal: korp_mono file missing ':::' in '[[[GEN:#'-expression (this should never happen)"
    )]
    MissingTripleColon,

    #[error(
        "Internal: Base64 decode error in base64-string in '[[[GEN:#'-expression (this should never happen)"
    )]
    Base64DecodeError(#[from] base64::DecodeError),

    #[error(
        "Internal: utf-8 error when stringifying base64-decoded data in '[[[GEN:#'-expression (this should never happen)"
    )]
    FromUtf8Error(#[from] std::string::FromUtf8Error),
}

#[derive(Debug, thiserror::Error)]
pub enum LoadFstError {
    #[error("hfst input stream error: {0}")]
    HfstError(#[from] HfstInputStreamError),

    #[error(".hfstol file does not contain exactly 1 transducer")]
    NotExactlyOneTransducer,
}

pub fn load_fst<P: AsRef<Path>>(path: P) -> Result<HfstTransducer, LoadFstError> {
    Ok(HfstInputStream::new(path.as_ref())?
        .read_only_transducer()
        .ok_or(LoadFstError::NotExactlyOneTransducer)?)
}

/// Find the word form in the file data `s`, for the current `GEN`-expr,
/// which starts at index `i`.
fn find_word_form(s: &str, i: usize) -> &str {
    // GENEXPR are found in these two places:
    // either
    // <sentence id="N">word_form TAB GENEXPR
    // or
    // <sentence id="N">word_form TAB lemma ... \n
    // word_form TAB GENEXPR
    //
    // so to find the word_form, look for the previous `>` or `\n`.
    let wordform_start_i = s[..i]
        .rfind(['>', '\n'])
        .expect("GENEXPR has a previous > or NL");
    &s[wordform_start_i..i - 1]
}

//
//
// Below: Functinos to change the input in various ways to try to
// get a generation.
//
//

const GENERATION_STRATEGIES: [fn(&str) -> Option<String>; 3] = [
    // The string as it comes from korp-mono, unaltered. MUST be
    // tried first.
    initial,
    try_replace_n_sg_nom_with_n_pl_nom,
    try_replace_inf_with_prfprc,
];

// The "initial" (do-nothing) strategy. Doesn't change anything in the input,
// and is always possible to do.
fn initial(s: &str) -> Option<String> {
    Some(s.to_string())
}

fn try_replace_n_sg_nom_with_n_pl_nom(s: &str) -> Option<String> {
    s.contains("N+Sg+Nom")
        .then_some(s.replace("N+Sg+Nom", "N+Pl+Nom"))
}

fn try_replace_inf_with_prfprc(s: &str) -> Option<String> {
    s.contains("+Inf").then_some(s.replace("+Inf", "+PrfPrc"))
}

//$ cat misc/NOT_GENERATED.txt |cut -f1|husme|cut -f2|sed 's/+Pl+\(...\)$/+Sg\1/'|sed -E "s/(Acc|Gen|Ill|Loc|Ess)$/Nom/"|grep -v "Attr$"|hdsme|grep "+?"|grep "+[^?]"|cut -d"+" -f1|uniq|wc -l
//    3605
// cat misc/NOT_GENERATED.txt |wc -l
// 11901
//fn try_replace_pl_non_nom_with_sg_nom(s: &str) -> Option<String> {
//    if s.contains("+Pl+") {
//        let i = s.rfind("+Pl+").unwrap();
//        let after_pl = &s[i..];
//        
//    } else {
//        None
//    }
//    s.contains("+Pl+").then_some(
//}

// THIS IS FOR Adjectives
fn try_replace_a_sg_nom_with_a_attr() {
    unimplemented!()
}

fn try_remove_err(s: &str) -> Option<String> {
    s.contains("+Err").then_some("hey".to_string())
}
