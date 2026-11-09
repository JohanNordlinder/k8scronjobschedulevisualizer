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

fn main() -> eframe::Result<()> {
    let mut opts = app::Options { kubeconfig: None, context: None, namespace: None, filter: None, demo: false };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next().unwrap_or_else(|| {
                eprintln!("{name} needs a value\n\n{USAGE}");
                std::process::exit(2)
            })
        };
        match arg.as_str() {
            "--kubeconfig" => opts.kubeconfig = Some(value("--kubeconfig")),
            "--context" => opts.context = Some(value("--context")),
            "-n" | "--namespace" => opts.namespace = Some(value("--namespace")),
            "-f" | "--filter" => opts.filter = Some(value("--filter")),
            "--demo" => opts.demo = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            other => {
                eprintln!("unknown argument '{other}'\n\n{USAGE}");
                std::process::exit(2);
            }
        }
    }

    let native = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("cronscope")
            .with_inner_size([1400.0, 860.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native("cronscope", native, Box::new(|cc| Ok(Box::new(app::App::new(cc, opts)))))
}
