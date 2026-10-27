# Tennis Bot

A Discord bot for simultaneous-bid coin tennis, written in Rust with
[Poise](https://docs.rs/poise/0.6.2/poise/) and Serenity.

## Run

Install stable Rust, then build and start the bot:

```sh
cargo build --release
export DISCORD_BOT_TOKEN='your-bot-token'
./target/release/tennis-bot
```

The token is read from the environment. A `.env` file is not loaded automatically.
For development, use `cargo run`. Set `RUST_LOG=tennis_bot=debug,warn` to adjust logging.
The worker entry in `Procfile` expects the release binary to have been built first.

In Discord's Developer Portal, enable **Message Content Intent** for prefix commands.
Invite the bot with the `bot` and `applications.commands` scopes, and grant **View
Channel**, **Send Messages**, **Read Message History**, and **Add Reactions** in the
channels where you play. Threads also require **Send Messages in Threads**.
Slash commands register globally at startup and may take time to appear.

## Commands

| Prefix | Slash | Action |
| --- | --- | --- |
| `!start` | `/start` | Start a game against the bot. |
| `!challenge @user` | `/challenge opponent` | Invite another person to play. |
| `!play 10` | `/play x:10` | Spend ten coins this round. |
| `!stop_game` | `/stop_game` | Stop your current game. |
| `!tennis_help` or `!help` | `/tennis_help` | Show commands and rules. |

Only the challenged player can accept with ✅ or decline with ❌. Challenges
expire after five minutes and are consumed once answered. Starting a game cancels
other pending challenges involving its human participants. Each person can play
one game at a time; the bot can play multiple independent games.

Both `!play` and `/play` work in multiplayer. Slash bets receive a private
acknowledgment; prefix commands are public Discord messages.
Use `/play` to keep your bid secret until both players have submitted.
Play and stop commands must be sent in the channel where the game started.
Each player gets one bet per round; a submitted bet cannot be replaced.

Completed rounds show the spending message, board, and optional winner
message separately. Bot games have no bet acknowledgment. Slash results use the
interaction API so they work wherever the slash command can respond. If result
delivery fails, the bot returns the completed round privately and warns that the
coins were already spent.

## Rules

Both players start with 64 coins and the ball in the middle of five cups. Each
round, each bids an integer between zero and their remaining coins. Both pay
their bids. The higher bidder moves the ball one cup toward the opponent; equal
bids leave it still. Player 1 pushes right and Player 2 pushes left.

The game ends when the ball falls off an edge or both players run out of coins.
When the coins run out, the ball's position decides the winner; the center is a
draw. One empty wallet alone does not end the game. Zero bids are allowed, so
players may need to use `stop_game` if neither is willing to spend.

Games and pending challenges live in memory and are lost on restart. Run a single
bot process for a token. Ctrl-C and SIGTERM shut down the Discord connection.

## Deployment

The included systemd service runs as the unprivileged `tennis-bot` system user,
with automatic startup at boot and restart on failure. Release binaries live under
`/opt/tennis-bot/releases/`; `/opt/tennis-bot/current` selects the active release.
The token is stored in root-owned `/etc/tennis-bot.env` with mode `0600`.

```sh
ssh user@your-server 'sudo systemctl status tennis-bot --no-pager'
ssh user@your-server 'sudo journalctl -u tennis-bot -n 50 --no-pager'
ssh user@your-server 'sudo systemctl restart tennis-bot'
```

`deploy/install.sh` installs a staged directory containing `tennis-bot`,
`tennis-bot.env`, and `tennis-bot.service`. Use a statically linked
`x86_64-unknown-linux-musl` release binary to avoid differences between the
development machine's libc and Ubuntu's libc. Never run a second local instance
using the production token.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

`game` owns validated round transitions and rendering, `strategy` owns the
original randomized bot policy, and `lobby` owns matches, pending bets, and
challenge lifetimes. These modules have no Discord dependency. `discord` adapts
commands and reactions to the lobby and never holds its state lock across a
network request. RNG injection keeps strategy tests reproducible.
