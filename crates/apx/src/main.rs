fn main() {
    apx_core::tracing_init::init_tracing();
    std::process::exit(apx_cli::run_cli(std::env::args().collect()));
}
