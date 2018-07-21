use clap::{App, Arg, ArgMatches, ErrorKind as ClapErrorKind};
use failure::{err_msg, Error, ResultExt};
use files::{create_file_recursively, fs_timestamp};
use rayon::ThreadPoolBuilder;
use runner;
use runner::SimulationRunner;
use simplelog::{CombinedLogger, Config, LevelFilter, SharedLogger, TermLogger, WriteLogger};
use std::collections::HashSet;
use std::default::Default;
use std::env::current_dir;
use std::fs::create_dir_all;
use std::path::{Path, PathBuf};

pub fn run() -> Result<(), Error> {
    let matches = new_app().get_matches_safe();

    match matches {
        // CLI arg parsing succeeded, unwrap the result and start loading and running simulation.
        Ok(ref matched) => {
            init_thread_pool(matched)?;

            let mut runner = init_simulation_runner(matched)?;

            {
                let spec = runner.spec();
                init_logging(matched, &spec.log, &fs_timestamp(runner.creation_time()))?;
            }

            // Log the description line-wise
            info!("Simulation ready, running…");
            for line in format!("{}", runner).lines() {
                info!("{}", line);
            }

            runner.run();
            info!("Finished simulation, done.");

            Ok(())
        }
        // CLI argument parsing either failed or the user just wanted help or version information
        Err(matches_error) => {
            init_logging_fallback()?;

            match matches_error.kind {
                // Those are in many cases not really errors but the user just did not want to run
                // anything right now. Exit the application successfully in these cases.
                // If use_stderr is not false, there were probably some subcommands missing and this
                // is in fact a real error that warrants unsuccessful exit.
                ClapErrorKind::HelpDisplayed | ClapErrorKind::VersionDisplayed
                    if !matches_error.use_stderr() =>
                {
                    println!("{}", matches_error.message);
                    Ok(())
                }
                // In all other cases, there was some sort of real error,
                // exit unsuccessfully in these cases.
                _ => Err(From::from(matches_error)),
            }
        }
    }
}

fn new_app<'a, 'b>() -> App<'a, 'b> {
    App::new("aitios")
        .version(crate_version!())
        .author("krachzack <hello@phstadler.com>")
        .about("Procedural weathering simulation on the command line with aitios")
        .arg(
            Arg::with_name("SIMULATION_SPEC_FILE")
                .help("Sets the path to the simulation config YAML file")
                .required(true)
                .validator(validate_simulation_spec)
                .index(1)
        )
        .arg(
            Arg::with_name("verbose")
                .short("v")
                .long("verbose")
                .multiple(true)
                .help("Activates verbose output")
        )
        .arg(
            Arg::with_name("log")
                .short("l")
                .long("log")
                .multiple(true)
                .takes_value(true)
                .min_values(0)
                .max_values(64)
                .value_name("LOG_FILE")
                .help("Specifies a file in which to log simulation progress")
        )
        .arg(
            Arg::with_name("threads")
                .short("t")
                .long("threads")
                .takes_value(true)
                .value_name("THREAD_COUNT")
                .validator(validate_thread_count)
                .help("Overrides thread pool size from number of virtual processors to the given thread count")
        )
}

fn init_thread_pool(matches: &ArgMatches) -> Result<(), Error> {
    if let Some(thread_count) = matches.value_of("THREAD_COUNT") {
        let thread_count = usize::from_str_radix(&thread_count, 10).unwrap(); // Can be unwrapped since validator checks this

        ThreadPoolBuilder::new()
            .num_threads(thread_count)
            .build_global()
            .context("Thread pool could not be set up with specified thread count.")?
    }
    Ok(())
}

fn init_simulation_runner(matches: &ArgMatches) -> Result<SimulationRunner, Error> {
    let spec_file_path = matches.value_of("SIMULATION_SPEC_FILE").unwrap(); // Can unwrap since is marked as required and parsing would have failed otherwise

    let runner = runner::load(spec_file_path).context(format!(
        "Simulation specification could not be loaded at {}",
        spec_file_path
    ))?;

    Ok(runner)
}

fn validate_simulation_spec(simulation_spec_file: String) -> Result<(), String> {
    if simulation_spec_file.is_empty() {
        return Err("Specified simulation spec file path is empty".into());
    }

    Ok(())
}

fn validate_thread_count(thread_count: String) -> Result<(), String> {
    usize::from_str_radix(&thread_count, 10)
        .map(|_| ())
        .map_err(|e| {
            format!(
                "Invalid thread count specified: {count}\nCause: {cause}",
                count = thread_count,
                cause = e
            )
        })
}

/// Initializes logging using the given argument matching result
/// and an optional additional log path.
///
/// If matching failed, tries to set up terminal only logging and
/// returns Ok(()) if successful, otherwise some Err value..
///
/// If matching was successful, tries to apply the logging config
/// and returns Ok(()) if successful, otherwise some Err value.
fn init_logging(
    matches: &ArgMatches,
    additional_log_path: &Option<PathBuf>,
    datetime: &str,
) -> Result<(), Error> {
    configure_logging(
        matches,
        additional_log_path
            .as_ref()
            .map(|p| p.to_string_lossy())
            .iter(),
        datetime,
    ).or_else(|_| init_logging_fallback())
}

/// Makes the only logger log to stdout as a fallback if logging setup did not
/// work out as planned.
fn init_logging_fallback() -> Result<(), Error> {
    TermLogger::init(LevelFilter::Warn, Default::default())
        .context("Could not install fallback terminal logger")?;

    Ok(())
}

fn configure_logging<I, S>(
    arg_matches: &ArgMatches,
    additional_logs: I,
    datetime: &str,
) -> Result<(), Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut loggers: Vec<Box<SharedLogger>> = vec![
        TermLogger::new(
            // Nothing => warn, -v => Info, -vv => Debug
            match arg_matches.occurrences_of("verbose") {
                0 => LevelFilter::Warn,
                1 => LevelFilter::Info,
                _ => LevelFilter::Debug,
            },
            Config::default(),
        ).ok_or(err_msg("Failed to set up logging to terminal."))?,
    ];

    let log_paths = canonical_log_file_paths(arg_matches, additional_logs, datetime)?;
    for log in log_paths.into_iter() {
        let log = create_file_recursively(log).context("Failed to create log file.")?;

        loggers.push(WriteLogger::new(LevelFilter::Debug, Config::default(), log));
    }

    CombinedLogger::init(loggers).context("Failed to set up combined logger.")?;

    Ok(())
}

fn canonical_log_file_paths<I, S>(
    arg_matches: &ArgMatches,
    additional_logs: I,
    datetime: &str,
) -> Result<HashSet<PathBuf>, Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut log_files = Vec::new();

    // First add log files explicitly specified with CLI args
    if let Some(log_file_args) = arg_matches.values_of("log") {
        log_files.extend(log_file_args.map(|a| log_arg_to_log_path(a, datetime)))
    }

    // If more log arguments were specified than log file names,
    // add exactly one additional log with a default log name.
    //
    // e.g. `aitios-cli sim.yml -l asdf.log -l` will log to both
    // asdf.log and the default log filename below the cwd.
    if (log_files.len() as u64) < (arg_matches.occurrences_of("log") as u64) {
        log_files.push(log_arg_to_log_path(
            &synthesize_datetime_log_filename(datetime),
            datetime,
        ));
    }

    // Finally add the additional log files, most likely coming from
    // simulation configuration.
    log_files.extend(
        additional_logs
            .into_iter()
            .map(|l| log_arg_to_log_path(l.as_ref(), datetime)),
    );

    // Canonicalize paths, filter out duplicates and abort on any errors
    log_files.into_iter().collect()
}

fn log_arg_to_log_path(arg: &str, datetime: &str) -> Result<PathBuf, Error> {
    // Replace {datetime} pattern with filename safe timestamp
    let arg = arg.replace("{datetime}", datetime);
    let path: &Path = arg.as_ref();

    if path.is_dir() {
        // If directory given, append default log filename
        let mut path = path.canonicalize()?;
        path.push(synthesize_datetime_log_filename(datetime));
        Ok(path)
    } else if path.is_file() {
        // Existing, regular file, return canonicalized form for overwrite
        Ok(path.canonicalize()?)
    } else {
        match path.parent() {
            // Relative one-level path returns Ok(""), just create the file,
            // under cwd since . is the implicit parent and always exists.
            Some(parent) if parent.as_os_str().is_empty() => {
                let mut new_path = current_dir()?.canonicalize()?;
                new_path.push(&arg);
                Ok(new_path)
            }
            // If immediate parent is an existing directory other than "",
            // canonicalize it, and append the final path component again.
            Some(parent) if parent.is_dir() => {
                let mut new_path = parent.canonicalize()?;
                new_path.push(path.file_name().unwrap());
                Ok(new_path)
            }
            // Ok, some nonexisting parent, try to create it
            Some(parent) => {
                create_dir_all(parent).unwrap();
                let mut new_path = parent.canonicalize()?;
                new_path.push(path.file_name().unwrap());
                Ok(new_path)
            }
            // Something about the path is wrong, stop trying
            _ => Err(format_err!("Log file path \"{}\" cannot be resolved", arg)),
        }
    }
}

/// Synthesize a default filename if -l or --log is passed without an actual filename.
fn synthesize_datetime_log_filename(datetime: &str) -> String {
    format!("aitios-log-{datetime}.log", datetime = datetime)
}

#[cfg(test)]
mod test {
    use super::*;
    use chrono::prelude::*;
    use std::iter;

    #[test]
    fn test_log_arg_with_datetime() {
        let time = Local::now();

        let expected = {
            let mut expected = current_dir().unwrap();
            expected.push(format!(
                "logovic-{datetime}.log",
                datetime = fs_timestamp(time)
            ));
            expected
        };

        let actual = log_arg_to_log_path("./logovic-{datetime}.log", &fs_timestamp(time)).unwrap();

        assert_eq!(expected.as_os_str().len(), actual.as_os_str().len());
        // when truncating until days, the test should always work,
        // except if we are very unlucky and midnight passes inbetween the
        // datetime formattings
        let len_until_day_of_month =
            current_dir().unwrap().to_str().unwrap().len() + "/logovic-2018-07-17T18_06_53".len();
        assert_eq!(
            expected
                .to_str()
                .unwrap()
                .to_string()
                .truncate(len_until_day_of_month),
            actual
                .to_str()
                .unwrap()
                .to_string()
                .truncate(len_until_day_of_month)
        )
    }

    #[test]
    fn test_log_arg_with_dot() {
        let time = Local::now();
        assert_eq!(
            {
                let mut expected = current_dir().unwrap();
                expected.push("loggy.log");
                expected
            },
            log_arg_to_log_path("./loggy.log", &fs_timestamp(time)).unwrap()
        )
    }

    #[test]
    fn test_log_arg_with_dotdot() {
        let time = Local::now();
        assert_eq!(
            {
                let mut expected = current_dir().unwrap();
                expected.pop();
                expected.push("loggy.log");
                expected
            },
            log_arg_to_log_path("../loggy.log", &fs_timestamp(time)).unwrap()
        )
    }

    #[test]
    fn no_log_file_when_no_log_arg() {
        let matches =
            new_app().get_matches_from(vec!["aitios-cli", "tests/examples/simulation.yml"]);

        let log_file_paths =
            canonical_log_file_paths(&matches, iter::empty::<&str>(), &fs_timestamp(Local::now()))
                .expect("Expect canonical log file calculation to succeed with no log switch");

        assert!(
            log_file_paths.is_empty(),
            "Expected no log files without log switch."
        )
    }

    #[test]
    fn test_default_log_name_added() {
        let matches =
            new_app().get_matches_from(vec!["aitios-cli", "tests/examples/simulation.yml", "-l"]);

        let log_file_paths =
            canonical_log_file_paths(&matches, iter::empty::<&str>(), &fs_timestamp(Local::now()))
                .expect(
                    "Expect canonical log file calculation to succeed with value-less log switch",
                );

        assert_eq!(
            1,
            log_file_paths.len(),
            "Expected exactly one log file path, namely the default one."
        );

        // when truncating until days, the test should always work,
        // except if we are very unlucky and midnight passes inbetween the
        // datetime formattings
        let time = Local::now();
        let len_until_day_of_month = current_dir().unwrap().to_str().unwrap().len()
            + "/".len()
            + synthesize_datetime_log_filename(&fs_timestamp(time)).len()
            - "_53.800963444+02_00.log".len();
        let mut expected = {
            let mut expected = current_dir().unwrap();
            expected.push(synthesize_datetime_log_filename(&fs_timestamp(time)));
            expected.to_str().unwrap().to_string()
        };
        let mut actual = log_file_paths
            .into_iter()
            .next()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        assert_eq!(
            expected.len(),
            actual.len(),
            "The default added log name has an unexpected length"
        );

        expected.truncate(len_until_day_of_month);
        actual.truncate(len_until_day_of_month);
        assert_eq!(
            expected, actual,
            "The default added log name looks different in the part until day of month"
        );
    }

    #[test]
    fn directory_as_log_path() {
        let matches = new_app().get_matches_from(vec![
            "aitios-cli",
            "tests/examples/simulation.yml",
            "-l",
            "..",
        ]);

        let log_file_paths =
            canonical_log_file_paths(&matches, iter::empty::<&str>(), &fs_timestamp(Local::now()))
                .expect("Expect canonical log file calculation to succeed");

        assert_eq!(
            1,
            log_file_paths.len(),
            "Expected 1 log file under the parent directory but got {:?}.",
            &log_file_paths
        );

        let mut parent_dir = current_dir().unwrap();
        parent_dir.pop();

        let mut parent_of_log_file_path = log_file_paths.iter().next().unwrap().clone();
        parent_of_log_file_path.pop();

        assert_eq!(
            parent_dir, parent_of_log_file_path,
            "Expected generated default log file to be in parent directory but got {:?}.",
            &log_file_paths
        );
    }

    #[test]
    fn create_intermediary_directories() {
        let pattern = "/tmp/{datetime}/some/dir/logggg.log";
        let timestamp = fs_timestamp(Local::now());
        let expected_log_path = pattern.replace("{datetime}", &timestamp);
        let matches = new_app().get_matches_from(vec![
            "aitios-cli",
            "tests/examples/simulation.yml",
            "-l",
            pattern,
        ]);

        let log_file_paths = canonical_log_file_paths(&matches, iter::empty::<&str>(), &timestamp)
            .expect("Expect canonical log file calculation to succeed");

        assert!(
            Path::new(&expected_log_path).parent().unwrap().is_dir(),
            "Expected canonical log file path lookup to create the necessary directories"
        );

        assert_eq!(1, log_file_paths.len());
    }

    #[test]
    fn test_duplicate_log_file_removal() {
        let matches = new_app().get_matches_from(vec![
            "aitios-cli",
            "tests/examples/simulation.yml",
            "-l",
            "log1.log",
            "-l",
            "././log1.log",
            "-l",
            ".",
            "-l",
        ]);

        let log_file_paths = canonical_log_file_paths(
            &matches,
            ["log2.log", "./log1.log", "."].iter(),
            &fs_timestamp(Local::now()),
        ).expect("Expect canonical log file calculation to succeed");

        assert_eq!(
            3,
            log_file_paths.len(),
            "Expected 3 log files (log1.log, log2.log, aitios-log-DATE.log), but found {:?}.",
            log_file_paths
        );
    }
}
