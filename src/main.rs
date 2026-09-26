use pay_lmm::{api, config::Config, service::Service};
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .init();
    let mut path = PathBuf::from("/etc/pay.lmm.best/config.toml");
    let mut check = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                path = PathBuf::from(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--config requires a path"))?,
                )
            }
            "--check-config" => check = true,
            "--version" => {
                println!("pay-lmm {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--help" | "-h" => {
                println!(
                    "pay-lmm [--config PATH] [--check-config] [--version]\nPayment gateway aggregation only; no payment processing or funds custody."
                );
                return Ok(());
            }
            _ => anyhow::bail!("unknown argument: {arg}"),
        }
    }
    let config = Config::load(&path)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()?;
    runtime.block_on(async move {
        let service=Service::new(config)?;
        if check {println!("Configuration, credentials and database validated.");return Ok(());}
        let listener=tokio::net::TcpListener::bind(&service.server.listen).await?;
        tracing::info!(listen=%service.server.listen,"payment protocol aggregator started; actual payment processing belongs to upstream providers");
        let (tx,rx)=tokio::sync::watch::channel(false);
        let worker=tokio::spawn(service.clone().worker(rx));
        axum::serve(listener,api::router(service)).with_graceful_shutdown(async move {shutdown().await;let _=tx.send(true);}).await?;
        worker.await?;
        Ok(())
    })
}
async fn shutdown() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
