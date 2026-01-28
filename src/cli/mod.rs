pub mod build;
pub mod dev;
pub mod flux;
pub mod init;
pub mod __generate_openapi;
pub mod bun;
pub mod components;
pub mod frontend;

pub async fn run_cli_async<F, Fut>(f: F) -> i32
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    match f().await {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}
