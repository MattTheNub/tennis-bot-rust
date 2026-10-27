use std::{future::Future, time::Instant};

use poise::serenity_prelude as serenity;
use rand::Rng;
use tokio::sync::Mutex;

use tennis_bot::{
    game::{MoveError, Outcome},
    lobby::{
        BetResponse, ChallengeId, ChallengeResponse, ChannelId, Lobby, LobbyError, MatchSnapshot,
        Opponent, PlayerId,
    },
};

pub type Error = anyhow::Error;
type Context<'a> = poise::Context<'a, Data, Error>;

#[derive(Default)]
pub struct Data {
    lobby: Mutex<Lobby>,
}

#[derive(Debug, thiserror::Error)]
#[error("could not publish a completed round: {source}")]
struct RoundDeliveryError {
    result: String,
    #[source]
    source: Error,
}

impl RoundDeliveryError {
    fn notice(&self) -> String {
        format!(
            "The round was played, but I couldn't publish the result to the channel. \
             Don't repeat your bet; the coins have already been spent.\n\n{}",
            self.result
        )
    }
}

const HELP: &str = "\
**Tennis Bot Commands**
`!challenge` - Sends a game request to another user
`!start` — Start a new game (you vs the bot)
`!play x` — Play a round by spending x coins (x must be <= your current coins)
`!stop_game` — Stop your current game

**Goal**: Move the ball off the opponent’s side to win or force a tie if it ends in the center.
**Rules**:
1. The ball starts in the middle of 5 cups.
2. Each round, you choose how many coins to spend (0 ≤ x ≤ your_coins).
3. Bot secretly does the same.
4. Higher spender pushes the ball one cup to their advantage.
5. Ties do not move the ball.
6. Game ends if the ball falls off an edge or if both players run out of coins.
";

pub fn options() -> poise::FrameworkOptions<Data, Error> {
    poise::FrameworkOptions {
        commands: vec![challenge(), start(), play(), stop_game(), tennis_help()],
        prefix_options: poise::PrefixFrameworkOptions {
            prefix: Some("!".into()),
            ..Default::default()
        },
        allowed_mentions: Some(serenity::CreateAllowedMentions::new().all_users(true)),
        on_error: |error| Box::pin(on_error(error)),
        event_handler: |ctx, event, _, data| Box::pin(on_event(ctx, event, data)),
        ..Default::default()
    }
}

async fn private_reply(ctx: Context<'_>, content: impl Into<String>) -> Result<(), Error> {
    ctx.send(
        poise::CreateReply::default()
            .content(content)
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

fn player(ctx: Context<'_>) -> PlayerId {
    PlayerId(ctx.author().id.get())
}

fn channel(ctx: Context<'_>) -> ChannelId {
    ChannelId(ctx.channel_id().get())
}

fn mention(id: PlayerId) -> String {
    format!("<@{}>", id.0)
}

fn command_error(author: PlayerId, command: &str, error: &LobbyError) -> String {
    let user = mention(author);
    match error {
        LobbyError::SelfChallenge => format!("{user}, you cannot challenge yourself!"),
        LobbyError::Busy(busy) if *busy == author => {
            format!("{user}, you already have a game in progress!")
        }
        LobbyError::NoGame if command == "stop_game" => {
            format!("{user}, you have no game in progress to stop.")
        }
        LobbyError::NoGame => format!("{user}, you have no active game. Use `!start` first."),
        LobbyError::Move(MoveError::InvalidBet { available }) => {
            format!("{user}, invalid move! Choose between 0 and {available}.")
        }
        _ => error.to_string(),
    }
}

fn board(snapshot: &MatchSnapshot) -> String {
    let players = snapshot.players.map(mention);
    snapshot.game.display([&players[0], &players[1]])
}

fn start_messages(snapshot: &MatchSnapshot, contest_reminder: bool) -> Vec<String> {
    let mut messages = Vec::new();
    if snapshot.opponent == Opponent::Bot {
        messages.push("Starting game with bot!".into());
    }
    if contest_reminder {
        messages.push("Why are you not doing spring contest?".into());
    }
    messages.push(format!(
        "Game started! {} (Player1) vs {} (Player2)\n",
        mention(snapshot.players[0]),
        mention(snapshot.players[1]),
    ));
    messages.push(board(snapshot));
    messages
}

fn round_messages(snapshot: &MatchSnapshot, bets: [u16; 2]) -> Vec<String> {
    let players = snapshot.players.map(mention);
    let bot_label = if snapshot.opponent == Opponent::Bot {
        " (Bot)"
    } else {
        ""
    };
    let mut messages = vec![
        format!(
            "{} spent {} coins.\n{}{bot_label} spent {} coins.",
            players[0], bets[0], players[1], bets[1],
        ),
        board(snapshot),
    ];
    match snapshot.game.outcome() {
        Some(Outcome::Winner(winner)) => {
            messages.push(format!("{} wins! 🏆", players[winner.index()]))
        }
        Some(Outcome::Draw) => messages.push("It’s a tie!".into()),
        None => {}
    }
    messages
}

async fn send_messages<F, Fut>(messages: &[String], mut send: F) -> Result<(), Error>
where
    F: FnMut(poise::CreateReply) -> Fut,
    Fut: Future<Output = Result<(), Error>>,
{
    for message in messages {
        send(
            poise::CreateReply::default()
                .content(message)
                .ephemeral(false),
        )
        .await?;
    }
    Ok(())
}

/// Send another player a tennis challenge.
#[poise::command(prefix_command, slash_command)]
async fn challenge(
    ctx: Context<'_>,
    #[description = "The player to challenge"] opponent: serenity::User,
) -> Result<(), Error> {
    ctx.defer().await?;
    if opponent.bot {
        ctx.say("Use `!start` or `/start` to play against the bot.")
            .await?;
        return Ok(());
    }
    let players = [player(ctx), PlayerId(opponent.id.get())];
    ctx.data()
        .lobby
        .lock()
        .await
        .check_challenge(players, Instant::now())?;
    let reply = ctx
        .say(format!("Waiting for {}'s response...", mention(players[1])))
        .await?;
    let message = reply.message().await?;
    let id = ChallengeId(message.id.get());
    let result =
        ctx.data()
            .lobby
            .lock()
            .await
            .add_challenge(id, players, channel(ctx), Instant::now());
    if let Err(error) = result {
        reply
            .edit(
                ctx,
                poise::CreateReply::default().content(format!("Challenge cancelled: {error}")),
            )
            .await?;
        return Ok(());
    }
    for emoji in ["✅", "❌"] {
        if let Err(error) = message
            .react(ctx, serenity::ReactionType::Unicode(emoji.into()))
            .await
        {
            let cancelled = ctx.data().lobby.lock().await.cancel_challenge(id);
            if cancelled {
                reply.edit(ctx, poise::CreateReply::default().content("Challenge cancelled: I couldn't add reactions. Check my Add Reactions and Read Message History permissions.")).await?;
            }
            return Err(error.into());
        }
    }
    Ok(())
}

/// Start a tennis game against the bot.
#[poise::command(prefix_command, slash_command)]
async fn start(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer().await?;
    let bot = PlayerId(ctx.framework().bot_id.get());
    let snapshot = ctx
        .data()
        .lobby
        .lock()
        .await
        .start_bot(player(ctx), bot, channel(ctx))?;
    let messages = start_messages(&snapshot, rand::rng().random_range(0..3) == 0);
    if let Err(error) = send_messages(&messages, |reply| async move {
        ctx.send(reply).await?;
        Ok(())
    })
    .await
    {
        ctx.data().lobby.lock().await.cancel_start(&snapshot);
        return Err(error);
    }
    Ok(())
}

/// Place a coin bet for this round.
#[poise::command(prefix_command, slash_command, ephemeral)]
async fn play(
    ctx: Context<'_>,
    #[description = "Coins to spend, from zero to your remaining balance"] x: i64,
) -> Result<(), Error> {
    // Resolve the short, synchronous state transition before choosing response
    // visibility: pending bets are private; completed rounds are public.
    let result =
        ctx.data()
            .lobby
            .lock()
            .await
            .submit_bet(player(ctx), channel(ctx), x, &mut rand::rng());
    match result? {
        BetResponse::Waiting => {
            private_reply(ctx, format!("{} placed their bet!", mention(player(ctx)))).await?
        }
        BetResponse::Round { snapshot, bets } => {
            let messages = round_messages(&snapshot, bets);
            let delivery = async {
                if snapshot.opponent == Opponent::Human {
                    private_reply(ctx, format!("{} placed their bet!", mention(player(ctx))))
                        .await?;
                } else {
                    ctx.defer().await?;
                }
                send_messages(&messages, |reply| async move {
                    ctx.send(reply).await?;
                    Ok(())
                })
                .await
            }
            .await;
            delivery.map_err(|source| RoundDeliveryError {
                result: messages.join("\n"),
                source,
            })?;
        }
    }
    Ok(())
}

/// Stop your current tennis game.
#[poise::command(prefix_command, slash_command)]
async fn stop_game(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer().await?;
    let snapshot = ctx
        .data()
        .lobby
        .lock()
        .await
        .stop(player(ctx), channel(ctx))?;
    ctx.say(format!(
        "Game between {} and {} has been stopped.",
        mention(snapshot.players[0]),
        mention(snapshot.players[1])
    ))
    .await?;
    Ok(())
}

/// Show the commands and rules of coin tennis.
#[poise::command(prefix_command, slash_command, aliases("help"))]
async fn tennis_help(ctx: Context<'_>) -> Result<(), Error> {
    ctx.say(HELP).await?;
    Ok(())
}

async fn on_event(
    ctx: &serenity::Context,
    event: &serenity::FullEvent,
    data: &Data,
) -> Result<(), Error> {
    let serenity::FullEvent::ReactionAdd {
        add_reaction: reaction,
    } = event
    else {
        return Ok(());
    };
    let Some(user) = reaction.user_id else {
        return Ok(());
    };
    let accept = match &reaction.emoji {
        serenity::ReactionType::Unicode(emoji) if emoji == "✅" => true,
        serenity::ReactionType::Unicode(emoji) if emoji == "❌" => false,
        _ => return Ok(()),
    };
    let response = data.lobby.lock().await.respond_to_challenge(
        ChallengeId(reaction.message_id.get()),
        PlayerId(user.get()),
        accept,
        Instant::now(),
    );
    let content = match response {
        Ok(Some(ChallengeResponse::Accepted(snapshot))) => {
            let messages = start_messages(&snapshot, rand::rng().random_range(0..3) == 0);
            for message in messages {
                if let Err(error) = reaction.channel_id.say(ctx, message).await {
                    data.lobby.lock().await.cancel_start(&snapshot);
                    return Err(error.into());
                }
            }
            return Ok(());
        }
        Ok(Some(ChallengeResponse::Declined(challenge))) => {
            format!("{} rejected your challenge!", mention(challenge.players[1]))
        }
        Ok(Some(ChallengeResponse::Expired)) => {
            "That challenge expired. Send a new one with `!challenge` or `/challenge`.".into()
        }
        Ok(None) => return Ok(()),
        Err(error) => format!("Challenge cancelled: {error}"),
    };
    reaction.channel_id.say(ctx, content).await?;
    Ok(())
}

async fn on_error(error: poise::FrameworkError<'_, Data, Error>) {
    match error {
        poise::FrameworkError::Command { error, ctx, .. } => {
            let message = if let Some(error) = error.downcast_ref::<LobbyError>() {
                command_error(player(ctx), &ctx.command().name, error)
            } else if let Some(error) = error.downcast_ref::<RoundDeliveryError>() {
                tracing::warn!(error = %error.source, channel = %ctx.channel_id(), guild = ?ctx.guild_id(), "completed round could not be published");
                error.notice()
            } else {
                tracing::error!(error = %error, command = %ctx.command().name, channel = %ctx.channel_id(), guild = ?ctx.guild_id(), "command failed");
                "I couldn't complete that request. Check the game state or try again.".into()
            };
            if let Err(error) = private_reply(ctx, message).await {
                tracing::warn!(%error, "could not send command error");
            }
        }
        error => {
            if let Err(error) = poise::builtins::on_error(error).await {
                tracing::error!(%error, "framework error handler failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::ready;

    use super::*;

    #[tokio::test]
    async fn bot_round_sends_spending_and_board_without_an_acknowledgment() {
        let mut snapshot = Lobby::default()
            .start_bot(PlayerId(1), PlayerId(2), ChannelId(3))
            .unwrap();
        snapshot.game.play_round([10, 5]).unwrap();
        let messages = round_messages(&snapshot, [10, 5]);
        let mut replies = Vec::new();
        send_messages(&messages, |reply| {
            replies.push(reply);
            ready(Ok(()))
        })
        .await
        .unwrap();

        assert_eq!(replies.len(), 2);
        assert!(replies.iter().all(|reply| reply.ephemeral == Some(false)));
        assert_eq!(
            replies[0].content.as_deref(),
            Some("<@1> spent 10 coins.\n<@2> (Bot) spent 5 coins.")
        );
        assert_eq!(
            replies[1].content.as_deref(),
            Some(board(&snapshot).as_str())
        );
    }

    #[tokio::test]
    async fn failed_public_result_retains_result_and_warns_against_repeating_bets() {
        let result = "<@1> spent 10 coins.\n<@2> spent 5 coins.\n54 coins remaining.";
        let mut attempts = 0;
        let source = send_messages(&[result.into()], |_| {
            attempts += 1;
            ready(Err(serenity::Error::Other("Missing Access").into()))
        })
        .await
        .unwrap_err();

        let error = RoundDeliveryError {
            result: result.into(),
            source,
        };
        assert_eq!(attempts, 1);
        assert!(error.notice().contains(result));
        assert!(error.notice().contains("Don't repeat your bet"));
    }

    #[test]
    fn original_start_and_winner_messages_remain_separate() {
        let mut snapshot = Lobby::default()
            .start_bot(PlayerId(1), PlayerId(2), ChannelId(3))
            .unwrap();
        let messages = start_messages(&snapshot, false);
        assert_eq!(
            messages,
            vec![
                "Starting game with bot!".to_owned(),
                "Game started! <@1> (Player1) vs <@2> (Player2)\n".into(),
                board(&snapshot)
            ]
        );
        assert_eq!(
            start_messages(&snapshot, true)[1],
            "Why are you not doing spring contest?"
        );
        snapshot.game.play_round([64, 64]).unwrap();
        assert_eq!(
            round_messages(&snapshot, [64, 64]).last().unwrap(),
            "It’s a tie!"
        );
    }

    #[test]
    fn original_validation_wording_is_preserved() {
        assert_eq!(
            command_error(PlayerId(1), "play", &LobbyError::NoGame),
            "<@1>, you have no active game. Use `!start` first."
        );
        assert_eq!(
            command_error(
                PlayerId(1),
                "play",
                &LobbyError::Move(MoveError::InvalidBet { available: 12 })
            ),
            "<@1>, invalid move! Choose between 0 and 12."
        );
        assert_eq!(
            command_error(PlayerId(1), "stop_game", &LobbyError::NoGame),
            "<@1>, you have no game in progress to stop."
        );
    }
}
