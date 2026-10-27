use std::cmp::Ordering;

use thiserror::Error;

pub const STARTING_COINS: u16 = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Player {
    One,
    Two,
}

impl Player {
    pub const fn index(self) -> usize {
        match self {
            Self::One => 0,
            Self::Two => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Winner(Player),
    Draw,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum MoveError {
    #[error("This game has already finished.")]
    Finished,
    #[error("Invalid bet: choose between 0 and {available} coins.")]
    InvalidBet { available: u16 },
}

/// The complete rules of coin tennis, independent of Discord and pending bets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Game {
    coins: [u16; 2],
    // Player one pushes toward -3 (visually right); player two toward +3 (left).
    position: i8,
    outcome: Option<Outcome>,
}

impl Default for Game {
    fn default() -> Self {
        Self {
            coins: [STARTING_COINS; 2],
            position: 0,
            outcome: None,
        }
    }
}

impl Game {
    pub fn coins(&self, player: Player) -> u16 {
        self.coins[player.index()]
    }

    pub fn position(&self) -> i8 {
        self.position
    }

    pub fn outcome(&self) -> Option<Outcome> {
        self.outcome
    }

    pub fn validate_bet(&self, player: Player, bet: i64) -> Result<u16, MoveError> {
        if self.outcome.is_some() {
            return Err(MoveError::Finished);
        }
        let available = self.coins(player);
        u16::try_from(bet)
            .ok()
            .filter(|&bet| bet <= available)
            .ok_or(MoveError::InvalidBet { available })
    }

    /// Validates both bets before changing anything, so errors are atomic.
    pub fn play_round(&mut self, bets: [i64; 2]) -> Result<(), MoveError> {
        let one = self.validate_bet(Player::One, bets[0])?;
        let two = self.validate_bet(Player::Two, bets[1])?;
        self.coins[0] -= one;
        self.coins[1] -= two;
        self.position += match one.cmp(&two) {
            Ordering::Greater => -1,
            Ordering::Less => 1,
            Ordering::Equal => 0,
        };

        if self.position.abs() == 3 || self.coins == [0, 0] {
            self.outcome = Some(match self.position.cmp(&0) {
                Ordering::Less => Outcome::Winner(Player::One),
                Ordering::Greater => Outcome::Winner(Player::Two),
                Ordering::Equal => Outcome::Draw,
            });
        }
        Ok(())
    }

    pub fn display(&self, players: [&str; 2]) -> String {
        let court = match self.position {
            -3 => "⚪  ⚪  ⚪  ⚪  ⚪  🎾   **(off the right side!)**".into(),
            3 => "🎾  ⚪  ⚪  ⚪  ⚪  ⚪   **(off the left side!)**".into(),
            position => (-2..=2)
                .rev()
                .map(|cup| if cup == position { "🎾" } else { "⚪" })
                .collect::<Vec<_>>()
                .join(" "),
        };
        format!(
            "{court}\n**{}'s Coins**: {} | **{}'s Coins**: {}\n**Ball Position**: {}",
            players[0], self.coins[0], players[1], self.coins[1], self.position,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_bids_spend_coins_without_moving_the_ball() {
        let mut game = Game::default();
        game.play_round([4, 4]).unwrap();
        assert_eq!(game.coins, [60, 60]);
        assert_eq!(game.position(), 0);
        assert_eq!(game.outcome(), None);
    }

    #[test]
    fn leaving_either_edge_ends_the_game_even_with_coins_left() {
        for (bets, position, winner, side) in [
            ([1, 0], -3, Player::One, "right"),
            ([0, 1], 3, Player::Two, "left"),
        ] {
            let mut game = Game::default();
            for _ in 0..3 {
                game.play_round(bets).unwrap();
            }
            assert_eq!(game.position(), position);
            assert_eq!(game.outcome(), Some(Outcome::Winner(winner)));
            assert!(
                game.display(["A", "B"])
                    .contains(&format!("off the {side}"))
            );
            assert_eq!(game.play_round([0, 0]), Err(MoveError::Finished));
        }
    }

    #[test]
    fn exhaustion_uses_the_ball_position_and_does_not_fake_an_edge_exit() {
        for (first, last, outcome) in [
            ([0, 0], [64, 64], Outcome::Draw),
            ([2, 1], [60, 62], Outcome::Winner(Player::One)),
            ([1, 2], [62, 60], Outcome::Winner(Player::Two)),
        ] {
            let mut game = Game::default();
            game.play_round(first).unwrap();
            game.play_round(first).unwrap();
            game.play_round(last).unwrap();
            assert_eq!(game.outcome(), Some(outcome));
            assert!(!game.display(["A", "B"]).contains("off the"));
        }
    }

    #[test]
    fn simultaneous_exhaustion_at_center_is_a_draw() {
        let mut game = Game::default();
        game.play_round([64, 64]).unwrap();
        assert_eq!(game.outcome(), Some(Outcome::Draw));
    }

    #[test]
    fn invalid_bets_do_not_change_any_state() {
        let mut game = Game::default();
        for bets in [[-1, 0], [0, -1], [65, 0], [0, 65], [i64::MAX, 0]] {
            let before = game.clone();
            assert!(matches!(
                game.play_round(bets),
                Err(MoveError::InvalidBet { .. })
            ));
            assert_eq!(game, before);
        }
    }

    #[test]
    fn one_empty_wallet_does_not_end_the_game() {
        let mut game = Game::default();
        game.play_round([64, 0]).unwrap();
        assert_eq!(game.outcome(), None);
        game.play_round([0, 1]).unwrap();
        assert_eq!(game.position(), 0);
    }

    #[test]
    fn board_moves_right_when_player_one_wins_a_round() {
        let mut game = Game::default();
        assert!(game.display(["A", "B"]).starts_with("⚪ ⚪ 🎾 ⚪ ⚪"));
        game.play_round([1, 0]).unwrap();
        assert!(game.display(["A", "B"]).starts_with("⚪ ⚪ ⚪ 🎾 ⚪"));
    }
}
