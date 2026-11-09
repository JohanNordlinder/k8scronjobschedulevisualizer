mod analysis;
mod app;
mod cluster;
mod demo;
mod model;
mod schedule;

const USAGE: &str = "\
cronscope - visualize CronJob schedules on an OpenShift/Kubernetes cluster

USAGE:
    cronscope [OPTIONS]

OPTIONS:
    --kubeconfig <PATH>   kubeconfig to use (default: $KUBECONFIG or ~/.kube/config)
    --context <NAME>      kubeconfig context (default: current-context)
    -n, --namespace <NS>  only this namespace (default: all namespaces)
    -f, --filter <TERMS>  name filter, e.g. \"backup, sync, !test\"
    --demo                start with generated demo data instead of a cluster
    -h, --help            show this help
";

#[derive(Debug)]
enum Command {
    Run(app::Options),
    Help,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut opts = app::Options { kubeconfig: None, context: None, namespace: None, filter: None, demo: false };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--kubeconfig" => opts.kubeconfig = Some(value("--kubeconfig")?),
            "--context" => opts.context = Some(value("--context")?),
            "-n" | "--namespace" => opts.namespace = Some(value("--namespace")?),
            "-f" | "--filter" => opts.filter = Some(value("--filter")?),
            "--demo" => opts.demo = true,
            "-h" | "--help" => return Ok(Command::Help),
            other => return Err(format!("unknown argument '{other}'")),
        }
    }
    Ok(Command::Run(opts))
}

fn main() -> eframe::Result<()> {
    let opts = match parse_args(std::env::args().skip(1)) {
        Ok(Command::Run(opts)) => opts,
        Ok(Command::Help) => {
            print!("{USAGE}");
            return Ok(());
        }
        Err(e) => {
            eprintln!("{e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    let native = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("cronscope")
            .with_inner_size([1400.0, 860.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native("cronscope", native, Box::new(|cc| Ok(Box::new(app::App::new(cc, opts)))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Command, String> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_args_means_defaults() {
        let Ok(Command::Run(o)) = parse(&[]) else { panic!() };
        assert!(o.kubeconfig.is_none() && o.context.is_none() && o.namespace.is_none() && o.filter.is_none());
        assert!(!o.demo);
    }

    #[test]
    fn all_options() {
        let Ok(Command::Run(o)) = parse(&[
            "--kubeconfig", "/tmp/kc", "--context", "prod", "-n", "billing", "-f", "backup, !test", "--demo",
        ]) else {
            panic!()
        };
        assert_eq!(o.kubeconfig.as_deref(), Some("/tmp/kc"));
        assert_eq!(o.context.as_deref(), Some("prod"));
        assert_eq!(o.namespace.as_deref(), Some("billing"));
        assert_eq!(o.filter.as_deref(), Some("backup, !test"));
        assert!(o.demo);

        let Ok(Command::Run(o)) = parse(&["--namespace", "a", "--filter", "b"]) else { panic!() };
        assert_eq!((o.namespace.as_deref(), o.filter.as_deref()), (Some("a"), Some("b")));
    }

    #[test]
    fn help_and_errors() {
        assert!(matches!(parse(&["-h"]), Ok(Command::Help)));
        assert!(matches!(parse(&["--demo", "--help"]), Ok(Command::Help)));
        assert_eq!(parse(&["--context"]).unwrap_err(), "--context needs a value");
        assert_eq!(parse(&["--bogus"]).unwrap_err(), "unknown argument '--bogus'");
    }
}
