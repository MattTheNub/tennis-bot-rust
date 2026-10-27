mod discord;

use anyhow::Context as _;
use poise::serenity_prelude as serenity;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "tennis_bot=info,warn".into()),
        )
        .init();

    let token = std::env::var("DISCORD_BOT_TOKEN")
        .context("Set DISCORD_BOT_TOKEN to your Discord bot token before starting tennis-bot")?;
    anyhow::ensure!(
        !token.trim().is_empty(),
        "DISCORD_BOT_TOKEN must not be empty"
    );

    let framework = poise::Framework::builder()
        .options(discord::options())
        .setup(|ctx, ready, framework| {
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                tracing::info!(user = %ready.user.name, commands = framework.options().commands.len(), "bot ready");
                Ok(discord::Data::default())
            })
        })
        .build();

    let intents = serenity::GatewayIntents::GUILDS
        | serenity::GatewayIntents::GUILD_MESSAGES
        | serenity::GatewayIntents::DIRECT_MESSAGES
        | serenity::GatewayIntents::GUILD_MESSAGE_REACTIONS
        | serenity::GatewayIntents::DIRECT_MESSAGE_REACTIONS
        | serenity::GatewayIntents::MESSAGE_CONTENT;
    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await
        .context("Could not create the Discord client")?;
    let shards = client.shard_manager.clone();
    tokio::select! {
        result = client.start() => result.context("Discord client stopped unexpectedly")?,
        result = shutdown_signal() => {
            result?;
            tracing::info!("shutting down");
            shards.shutdown_all().await;
        }
    }
    Ok(())
}

async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}
