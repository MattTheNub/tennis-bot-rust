use rand::Rng;

use crate::game::{Game, Player, STARTING_COINS};

/// The original randomized `new_strat` policy, with every bid clamped to a
/// legal amount. The RNG is injected so matches and tests can be reproduced.
pub fn choose_bet(game: &Game, rng: &mut impl Rng) -> u16 {
    if game.outcome().is_some() {
        return 0;
    }
    bounded_bet(
        game.coins(Player::Two),
        game.coins(Player::One),
        game.position(),
        rng,
    )
}

fn bounded_bet(bot: u16, opponent: u16, position: i8, rng: &mut impl Rng) -> u16 {
    let bot = i32::from(bot);
    let opponent = i32::from(opponent);
    let bid = base_bet(bot, opponent, position, rng);
    bid.clamp(0, bot.min(opponent + 1)) as u16
}

fn base_bet(bot: i32, opponent: i32, position: i8, rng: &mut impl Rng) -> i32 {
    let delta = bot - opponent;
    if delta > 0 && opponent == 0 {
        return 1;
    }

    let guaranteed_push = bot / (3 - i32::from(position));
    if guaranteed_push > opponent {
        return guaranteed_push;
    }

    match position {
        0 => {
            if opponent == i32::from(STARTING_COINS) {
                return match rng.random_range(0..=10) {
                    0..=3 => 0,
                    4 => rng.random_range(0..=4),
                    5 => rng.random_range(0..=8),
                    6 => rng.random_range(0..=15),
                    _ => rng.random_range(15..=21),
                };
            }
            let roll = rng.random_range(0..=10);
            if delta > 0 && 5 * delta <= bot && rng.random_bool(0.5) {
                return 2 * delta;
            }
            match roll {
                0..=1 => 0,
                2..=6 if opponent >= 55 => rng.random_range(10..=20),
                2..=6 if opponent >= 40 => rng.random_range(5..=10),
                _ => rng.random_range(0..=(bot + 2) / 3),
            }
        }
        -1 | 1 => match rng.random_range(0..=10) {
            0..=4 if opponent >= 40 => rng.random_range(10..=20),
            0..=4 if opponent >= 20 => rng.random_range(5..=10),
            0..=4 => rng.random_range(0..=8.min(bot)),
            5..=8 => rng.random_range(0..=(bot + 3) / 4),
            _ => rng.random_range(0..=delta.abs()),
        },
        -2 => match rng.random_range(0..=10) {
            0..=1 if bot > opponent => opponent + 1,
            0..=6 => bot.min(rng.random_range(delta..=delta + 5)),
            10 => bot.min(1),
            _ => rng.random_range((bot + 1) / 2..=bot),
        },
        2 => match rng.random_range(0..=10) {
            0..=3 => bot,
            4..=6 => bot / 2,
            7..=9 => 0,
            _ => rng.random_range(0..=bot),
        },
        _ => unreachable!("an active game keeps the ball on the court"),
    }
}

#[cfg(test)]
mod tests {
    use rand::{SeedableRng, rngs::StdRng};

    use super::*;

    #[test]
    fn policy_covers_every_wallet_and_court_position_without_illegal_bets() {
        let mut rng = StdRng::seed_from_u64(42);
        for bot in 0..=64 {
            for opponent in 0..=64 {
                for position in -2..=2 {
                    for _ in 0..32 {
                        let bid = bounded_bet(bot, opponent, position, &mut rng);
                        assert!((0..=bot).contains(&bid));
                        assert!(bid <= opponent + 1);
                    }
                }
            }
        }
    }

    #[test]
    fn an_empty_opponent_wallet_costs_exactly_one_coin() {
        let mut game = Game::default();
        game.play_round([64, 0]).unwrap();
        assert_eq!(choose_bet(&game, &mut StdRng::seed_from_u64(1)), 1);
    }

    #[test]
    fn randomized_complete_games_preserve_coin_budgets_and_finish() {
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..1_000 {
            let mut game = Game::default();
            // Force the human to spend at least one coin when available;
            // once empty, the bot pushes for one coin each remaining round.
            for _ in 0..70 {
                if game.outcome().is_some() {
                    break;
                }
                let available = game.coins(Player::One);
                let human = if available == 0 {
                    0
                } else {
                    rng.random_range(1..=available)
                };
                let bot = choose_bet(&game, &mut rng);
                assert!(bot <= game.coins(Player::Two));
                game.play_round([i64::from(human), i64::from(bot)]).unwrap();
            }
            assert!(game.outcome().is_some());
        }
    }
}
