use clap::Parser as _;
use clap_cargo::{Features, Manifest, Workspace};
use clap_verbosity_flag::{InfoLevel, Verbosity};
use color_eyre::eyre::{bail, Context as _};
use itertools::Itertools as _;
use pulldown_cmark::{BrokenLink, CodeBlockKind, CowStr, Event, Tag};
use std::{
    ffi::OsStr,
    fs::File,
    io::{self, stdout},
    path::PathBuf,
};
use tracing::{debug, warn};

#[derive(Debug, clap::Parser)]
#[command(name = "cargo", bin_name = "cargo")]
enum ArgsWrapper {
    #[command(about, version)]
    ExtractReadme(Args),
}

#[derive(Debug, clap::Parser)]
struct Args {
    #[command(flatten)]
    verbosity: Verbosity<InfoLevel>,
    #[command(flatten)]
    manifest: Manifest,
    #[command(flatten)]
    workspace: Workspace,
    #[command(flatten)]
    features: Features,
    #[arg(short, long, default_value = "nightly")]
    toolchain: String,

    /// File to write to
    #[arg(short, long)]
    output: Option<PathBuf>,
}

#[test]
fn args() {
    <Args as clap::CommandFactory>::command().debug_assert();
}

fn main() -> color_eyre::Result<()> {
    let Args {
        verbosity: _,
        manifest,
        workspace,
        features,
        toolchain,
        output,
    } = get_args_and_setup_logging()?;

    let mut metadata = manifest.metadata();
    features.forward_metadata(&mut metadata);
    let metadata = metadata
        .exec()
        .context("couldn't execute cargo metadata command")?;

    let (selected, excluded) = workspace.partition_packages(&metadata);
    {
        let selected = selected
            .iter()
            .map(|package| &package.name)
            .collect::<Vec<_>>();
        debug!(?selected, excluded = excluded.len(), "packages")
    }

    let Features {
        all_features,
        no_default_features,
        features,
        ..
    } = features;

    let output: Box<dyn io::Write> = match output {
        Some(path) if path == OsStr::new("-") => Box::new(stdout()),
        None => Box::new(stdout()),
        Some(path) => Box::new(File::create(path).context("couldn't open output file")?),
    };

    let mut json_builder = rustdoc_json::Builder::default();
    if let Some(path) = manifest.manifest_path {
        json_builder = json_builder.manifest_path(path)
    }
    for package in &workspace.package {
        json_builder = json_builder.package(package)
    }

    let json_path = json_builder
        .all_features(all_features)
        .no_default_features(no_default_features)
        .features(features)
        .toolchain(toolchain)
        .build()?;

    let krate = serde_json::from_reader::<_, rustdoc_types::Crate>(
        File::open(json_path).context("couldn't open file containing rustdoc-json")?,
    )
    .context("couldn't deserialize rustdoc json")?;

    let Some(root_docs) = &krate.index[&krate.root].docs else {
        bail!("root does not have any documentation")
    };
    let mut state = pulldown_cmark_to_cmark::State::default();

    fmt2io::write(output, |output| {
        for mut event in pulldown_cmark::Parser::new_with_broken_link_callback(
            root_docs,
            pulldown_cmark::Options::empty(),
            Some(&mut |BrokenLink {
                           span,
                           link_type,
                           reference,
                       }| {
                warn!(?span, ?link_type, ?reference, "broken_link");
                None
            }),
        ) {
            debug!(?event);
            match &event {
                Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(hint)))
                    if hint.as_ref() == "" =>
                {
                    event = Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(CowStr::Borrowed(
                        "rust",
                    ))))
                }
                Event::Text(code_block) if state.is_in_code_block => {
                    let stripped = code_block
                        .lines()
                        .filter(|line| !line.starts_with("# "))
                        .join("\n")
                        .into_boxed_str();
                    event = Event::Text(CowStr::Boxed(stripped))
                }
                _ => (),
            }

            state = pulldown_cmark_to_cmark::cmark_resume(
                std::iter::once(event),
                &mut *output,
                Some(state),
            )?;
        }
        state.finalize(output)?;
        Ok(())
    })
    .context("couldn't write output")?;

    Ok(())
}

/// Parse args, gracefully exiting the process if parsing fails.
/// # Panics
/// - If global logger has already been setup
fn get_args_and_setup_logging() -> color_eyre::Result<Args> {
    color_eyre::install()?;
    let ArgsWrapper::ExtractReadme(args) = ArgsWrapper::parse();
    let env_filter = tracing_subscriber::EnvFilter::builder()
        .with_default_directive({
            // TODO(aatifsyed): default directive should be our crate
            use tracing_subscriber::filter::LevelFilter;
            match args.verbosity.log_level() {
                Some(log::Level::Error) => LevelFilter::ERROR,
                Some(log::Level::Warn) => LevelFilter::WARN,
                Some(log::Level::Info) => LevelFilter::INFO,
                Some(log::Level::Debug) => LevelFilter::DEBUG,
                Some(log::Level::Trace) => LevelFilter::TRACE,
                None => LevelFilter::OFF,
            }
            .into()
        })
        .from_env()
        .context("couldn't parse RUST_LOG environment variable")?;
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stderr)
        .init();
    debug!(?args);
    Ok(args)
}
