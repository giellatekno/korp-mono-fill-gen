//! Read all xml files in korp_mono/, and replace all occurences of
//! `[[[GEN:<INNER>:::<READING>]]]`
//! with the generated text from passing INNER to the generator fst.

use clap::Parser;
use gtcorpusutil::Root;
use hfst::HfstTransducer;
use std::path::PathBuf;
use std::time::Instant;

/// Read all korp_mono xml files, and replace `[[[GEN:<inner>]]]` with
/// the generated text. To do this, send all the `<inner>` text to the
/// generator for that language.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Language you want to process, in 3-letter ISO-639-3 code, e.g.
    /// `nob` or `sme`.
    language: String,

    /// Directory where the corpus directories are stored.
    ///
    /// It is customary to keep all `corpus-xxx[...]` directories in a
    /// common directory, and often this directory is named `giellalt` (the
    /// same as the organiztion name is on github).
    ///
    /// If `gut` is installed on the system, gut's root will be used
    /// as the root.
    #[arg(long = "root")]
    root: Option<PathBuf>,

    /// Just do a dry run, do not write update data into the korp_mono file.
    #[arg(long)]
    dry_run: bool,

    /// Path to the generator fst.
    ///
    /// By default it uses the first 'generator-gt-norm.hfstol' it
    /// finds on the system.
    ///
    /// Paths searched are: `(~/.local | /usr/ | /usr/local/)/share/giella/{lang}/`
    #[arg(long)]
    generator_fst: Option<PathBuf>,

    /// Report timings on stderr.
    #[arg(long)]
    timings: bool,
}

/// Special-case: Words on the form "x%", where x is a number.
/// The lemma should just be the exact word form, e.g. "50%" or, if the original
/// contained spaces, then with the spaces included, such as "2 %".
fn handle_number_pct(gen_str: &str, fst: &HfstTransducer) -> Option<Vec<String>> {
    gen_str.strip_prefix("%+").map(|s| vec![s.to_string()])
}

fn find_files(root: gtcorpusutil::Root, lang: &str) -> Vec<gtcorpusutil::KorpMonoFilePath> {
    root.corpora()
        .filter(|corpus| corpus.corpus_name.lang == lang)
        .flat_map(|corpus| corpus.to_korp_mono().files().collect::<Vec<_>>())
        .collect()
}

/// Finds which directory to use as the root, given the `root` argument
fn find_root(root: Option<PathBuf>) -> anyhow::Result<Root> {
    match root {
        Some(path) => Ok(Root::from(gtcorpusutil::path_rel2abs_with_cwd(path)?)),
        None => match Root::from_gut_config() {
            Ok(root) => Ok(root),
            Err(e) => Err(anyhow::anyhow!(
                "failed to get gut root directory:\n{e}\n\
                    hint: you can specify the corpus root with the --corpus-root\
                    argument"
            )),
        },
    }
}

fn find_generator_fst(generator_fst: Option<PathBuf>, lang: &str) -> anyhow::Result<PathBuf> {
    const GENERATOR: &str = "generator-gt-norm.hfstol";

    match generator_fst {
        Some(path) => Ok(path),
        None => match gtcorpusutil::find_lang_resource(&lang, GENERATOR) {
            Some(path) => Ok(path),
            None => Err(anyhow::anyhow!("no {GENERATOR} found for lang {lang}")),
        }
    }
}

fn main() -> anyhow::Result<()> {
    let Args {
        language: lang,
        root,
        generator_fst,
        timings,
        dry_run,
        ..
    } = Args::parse();

    let root = find_root(root)?;
    let files = find_files(root, &lang);
    let nfiles = files.len();
    let generator_fst = find_generator_fst(generator_fst, &lang)?;
    let processor = korp_mono_fill_gen::Processor::new(generator_fst)?;

    println!("Korp-mono-fill-gen starting, {nfiles} files to process..",);

    let t0 = Instant::now();
    let mut ngen = 0;
    let mut nok = 0;

    for file in files {
        let p = file.to_path_buf();
        match processor.process(&file) {
            Ok((updated_file_data, gen_statuses)) => {
                if !dry_run {
                    std::fs::write(p, updated_file_data.as_bytes())?;
                }

                for st in gen_statuses {
                    ngen += 1;
                    let msg = if st.is_success() {
                        "OK"
                    } else {
                        "ERROR"
                    };

                    println!("----- {msg} -----");
                    println!("Word form: {}", st.word_form.trim());
                    println!("Analysis:");
                    println!("{}", st.reading);

                    if st.is_success() {
                        nok += 1;
                        for (i, attempt) in st.attempts.iter().enumerate() {
                            println!("{}    {}", i + 1, attempt.input);
                        }
                    } else {
                        println!("Attempts sent to generator (that did not generate anything):\n");
                        for (i, attempt) in st.attempts.iter().enumerate() {
                            println!("{}    {}", i + 1, attempt.input);
                        }
                    }
                    println!("----- /{msg} -----");
                }
            }
            Err(e) => {
                eprintln!("{e}");
                continue;
            }
        }
    }

    println!("{nfiles} files, {ngen} GENs, {nok} generated ok");

    if timings {
        let dur = t0.elapsed();
        eprintln!("{dur:?}");
    }

    Ok(())
}
