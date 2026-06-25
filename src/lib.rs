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
    result: Vec<String>,
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
        file: KorpMonoFilePath,
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

            // first append everything up to the last match
            new_s.push_str(&s[i..m.start()]);

            let (gen_str, reading) = Self::split_gen(m.as_str())?;
            statuses.push(GenStatus::new(word_form, reading));
            let status = statuses.last_mut().unwrap();

            if do_lookup(gen_str, &mut new_s, &fst, status) {
                continue;
            }

            // couldn't generate lemma, so try to change the generation
            // string in various ways

            // first check if this strategy even makes sense to try..
            if let Some(s) = try_replace_n_sg_nom_with_n_pl_nom(gen_str) {
                // if so, do a new lookup with this replacement
                if do_lookup(&s, &mut new_s, &fst, status) {
                    // if now succesful, we can move to next GEN
                    continue;
                }
            };

            if let Some(s) = try_replace_inf_with_prfprc(gen_str) {
                if do_lookup(&s, &mut new_s, &fst, status) {
                    continue;
                }
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

fn try_replace_n_sg_nom_with_n_pl_nom(s: &str) -> Option<String> {
    s.contains("N+Sg+Nom")
        .then_some(s.replace("N+SG+Nom", "N+Pl+Nom"))
}

// THIS IS FOR Adjectives
fn try_replace_a_sg_nom_with_a_attr() {
    unimplemented!()
}

fn try_replace_inf_with_prfprc(s: &str) -> Option<String> {
    s.contains("+Inf").then_some(s.replace("+Inf", "+PrfPrc"))
}
