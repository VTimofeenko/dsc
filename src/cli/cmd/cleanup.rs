use clap::Parser;
use snafu::{ResultExt, Snafu};
use std::path::{Path, PathBuf};

use super::{Cmd, Context};
use crate::http::Error as HttpError;
use crate::util::{digest, file};
use crate::{
    cli::opts::{EndpointOpts, FileAction},
    util::file::FileActionResult,
};
use crate::{cli::sink::Error as SinkError, http::payload::BasicResult};

/// Cleans directories from files that are in Docspell.
///
/// Traverses one or more directories and check each file whether it
/// exists in Docspell. If so, it can be deleted or moved to another
/// place.
///
/// If you want to upload all files that don't exists in some
/// directory, use the `upload` command.
///
/// When using the integration endpoint and a collective is not
/// specified, it will be guessed from the first subdirectory of the
/// directory that is specified.
#[derive(Parser, Debug)]
pub struct Input {
    #[clap(flatten)]
    pub endpoint: EndpointOpts,

    #[clap(flatten)]
    pub action: FileAction,

    /// Each file is printed.
    #[arg(long)]
    pub dry_run: bool,

    /// One or more files/directories to check. Directories are
    /// traversed recursively.
    #[arg(required = true, num_args = 1)]
    pub files: Vec<PathBuf>,
}

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("An http error occurred: {}", source))]
    HttpClient { source: HttpError },

    #[snafu(display("Error writing data: {}", source))]
    WriteResult { source: SinkError },

    #[snafu(display("Pattern error: {}", source))]
    Pattern { source: glob::PatternError },

    #[snafu(display("Glob error: {}", source))]
    Glob { source: glob::GlobError },

    #[snafu(display("Cannot delete or move: {}", source))]
    FileActionError { source: std::io::Error },

    #[snafu(display("No action given. Use --move or --delete."))]
    NoAction,

    #[snafu(display("A collective was not found and was not specified"))]
    NoCollective,

    #[snafu(display("The target '{}' is not a directory", path.display()))]
    TargetNotDirectory { path: PathBuf },

    #[snafu(display("Calculating digest of file {} failed: {}", path.display(), source))]
    DigestFail {
        source: std::io::Error,
        path: PathBuf,
    },
}

impl Cmd for Input {
    type CmdError = Error;

    fn exec(&self, ctx: &Context) -> Result<(), Error> {
        check_args(self)?;
        let result = cleanup(self, ctx)?;
        ctx.write_result(BasicResult {
            success: true,
            message: format!("Cleaned up files: {}", result),
        })
        .context(WriteResultSnafu)?;
        Ok(())
    }
}

fn check_args(args: &Input) -> Result<(), Error> {
    match &args.action.move_to {
        Some(path) => {
            if path.is_dir() {
                Ok(())
            } else {
                Err(Error::TargetNotDirectory { path: path.clone() })
            }
        }
        None => {
            if args.action.delete {
                Ok(())
            } else {
                Err(Error::NoAction)
            }
        }
    }
}

fn cleanup(args: &Input, ctx: &Context) -> Result<u32, Error> {
    let mut counter = 0;
    for file in &args.files {
        if file.is_dir() {
            let pattern = file.join("**/*").display().to_string();
            for child in glob::glob(&pattern).context(PatternSnafu)? {
                let cf = child.context(GlobSnafu)?;
                if cf.is_file() {
                    counter += cleanup_and_report(&cf, Some(file), args, ctx)?;
                }
            }
        } else {
            counter += cleanup_and_report(file, None, args, ctx)?;
        }
    }
    Ok(counter)
}

fn cleanup_and_report(
    file: &Path,
    root: Option<&PathBuf>,
    args: &Input,
    ctx: &Context,
) -> Result<u32, Error> {
    eprintln!("Check file: {}", file.display());
    let exists = check_file_exists(file, root, &args.endpoint, ctx)?;
    log::debug!("Checking file: {} (exists: {})", file.display(), exists);
    if exists {
        eprint!(" - exists: ");
        if !args.dry_run {
            let res = args.action.execute(file, root).context(FileActionSnafu)?;
            log::debug!("Action executed: {:?}", res);
            match res {
                FileActionResult::Deleted(_p) => {
                    eprintln!("deleted.");
                    return Ok(1);
                }
                FileActionResult::Moved(_p) => {
                    eprintln!("moved.");
                    return Ok(1);
                }
                FileActionResult::Nothing => {
                    log::error!("No file action defined. This should not happen, because user was able to not define it");
                    return Ok(0);
                }
            }
        } else {
            eprintln!("{}", exists);
            return Ok(1);
        }
    }
    Ok(0)
}

fn check_file_exists(
    path: &Path,
    root: Option<&PathBuf>,
    opts: &EndpointOpts,
    ctx: &Context,
) -> Result<bool, Error> {
    let dirs: Vec<PathBuf> = match root {
        Some(d) => vec![d.clone()],
        None => vec![],
    };

    let fauth = opts
        .to_file_auth(ctx, &|| {
            file::collective_from_subdir(path, &dirs).unwrap_or(None)
        })
        .ok_or(Error::NoCollective)?;

    let hash = digest::digest_file_sha256(path).context(DigestFailSnafu { path })?;
    let result = ctx
        .client
        .file_exists(hash, &fauth)
        .context(HttpClientSnafu)?;

    Ok(result.exists)
}
