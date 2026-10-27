use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use rand::Rng;
use thiserror::Error;

use crate::{
    game::{Game, MoveError, Player},
    strategy,
};

pub const CHALLENGE_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PlayerId(pub u64);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ChannelId(pub u64);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ChallengeId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Opponent {
    Bot,
    Human,
}

/// Safe to publish: unpaired bets never appear in a snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatchSnapshot {
    id: u64,
    pub players: [PlayerId; 2],
    pub channel: ChannelId,
    pub opponent: Opponent,
    pub game: Game,
}

#[derive(Debug)]
struct ActiveMatch {
    snapshot: MatchSnapshot,
    bets: [Option<u16>; 2],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Challenge {
    pub players: [PlayerId; 2],
    pub channel: ChannelId,
    created_at: Instant,
}

#[derive(Debug, Eq, PartialEq)]
pub enum ChallengeResponse {
    Accepted(MatchSnapshot),
    Declined(Challenge),
    Expired,
}

#[derive(Debug, Eq, PartialEq)]
pub enum BetResponse {
    Waiting,
    Round {
        snapshot: MatchSnapshot,
        bets: [u16; 2],
    },
}

#[derive(Debug, Eq, Error, PartialEq)]
pub enum LobbyError {
    #[error("You cannot challenge yourself!")]
    SelfChallenge,
    #[error("<@{}> already has a game in progress!", .0.0)]
    Busy(PlayerId),
    #[error("You have no active game. Use `!start` or `/start` first.")]
    NoGame,
    #[error("Play this game in <#{}>.", .0.0)]
    WrongChannel(ChannelId),
    #[error("You already placed a bet for this round. Wait for your opponent.")]
    AlreadyBet,
    #[error("You already have a pending challenge. It expires after five minutes.")]
    PendingChallenge,
    #[error(transparent)]
    Move(#[from] MoveError),
}

/// All state transitions are synchronous. Callers hold one short lock and
/// release it before making any Discord request.
#[derive(Debug, Default)]
pub struct Lobby {
    next_match_id: u64,
    matches: HashMap<PlayerId, ActiveMatch>,
    seats: HashMap<PlayerId, PlayerId>,
    challenges: HashMap<ChallengeId, Challenge>,
}

impl Lobby {
    pub fn game_for(&self, player: PlayerId) -> Option<&MatchSnapshot> {
        self.seats
            .get(&player)
            .and_then(|key| self.matches.get(key))
            .map(|game| &game.snapshot)
    }

    pub fn start_bot(
        &mut self,
        player: PlayerId,
        bot: PlayerId,
        channel: ChannelId,
    ) -> Result<MatchSnapshot, LobbyError> {
        self.start([player, bot], channel, Opponent::Bot)
    }

    fn ensure_available(&self, players: &[PlayerId]) -> Result<(), LobbyError> {
        for &player in players {
            if self.seats.contains_key(&player) {
                return Err(LobbyError::Busy(player));
            }
        }
        Ok(())
    }

    fn start(
        &mut self,
        players: [PlayerId; 2],
        channel: ChannelId,
        opponent: Opponent,
    ) -> Result<MatchSnapshot, LobbyError> {
        if players[0] == players[1] {
            return Err(LobbyError::SelfChallenge);
        }
        let humans = match opponent {
            Opponent::Bot => &players[..1],
            Opponent::Human => &players[..],
        };
        self.ensure_available(humans)?;
        self.next_match_id = self
            .next_match_id
            .checked_add(1)
            .expect("match IDs exhausted");
        let snapshot = MatchSnapshot {
            id: self.next_match_id,
            players,
            channel,
            opponent,
            game: Game::default(),
        };
        self.matches.insert(
            players[0],
            ActiveMatch {
                snapshot: snapshot.clone(),
                bets: [None; 2],
            },
        );
        for &player in humans {
            self.seats.insert(player, players[0]);
        }
        self.challenges.retain(|_, challenge| {
            !challenge
                .players
                .iter()
                .any(|player| humans.contains(player))
        });
        Ok(snapshot)
    }

    /// Undo a failed announcement only if nobody has played in this match.
    /// A delayed HTTP failure must not remove a newer or already-played match.
    pub fn cancel_start(&mut self, snapshot: &MatchSnapshot) -> bool {
        let key = snapshot.players[0];
        let untouched = self
            .matches
            .get(&key)
            .is_some_and(|active| active.snapshot == *snapshot && active.bets == [None; 2]);
        if untouched {
            self.remove_match(key);
        }
        untouched
    }

    pub fn check_challenge(
        &mut self,
        players: [PlayerId; 2],
        now: Instant,
    ) -> Result<(), LobbyError> {
        self.challenges
            .retain(|_, challenge| now.duration_since(challenge.created_at) < CHALLENGE_TTL);
        if players[0] == players[1] {
            return Err(LobbyError::SelfChallenge);
        }
        self.ensure_available(&players)?;
        if self
            .challenges
            .values()
            .any(|challenge| challenge.players[0] == players[0])
        {
            return Err(LobbyError::PendingChallenge);
        }
        Ok(())
    }

    pub fn add_challenge(
        &mut self,
        id: ChallengeId,
        players: [PlayerId; 2],
        channel: ChannelId,
        now: Instant,
    ) -> Result<(), LobbyError> {
        // Recheck after the challenge message has been sent: either player may
        // have started a different game while Discord was responding.
        self.check_challenge(players, now)?;
        self.challenges.insert(
            id,
            Challenge {
                players,
                channel,
                created_at: now,
            },
        );
        Ok(())
    }

    pub fn cancel_challenge(&mut self, id: ChallengeId) -> bool {
        self.challenges.remove(&id).is_some()
    }

    pub fn respond_to_challenge(
        &mut self,
        id: ChallengeId,
        player: PlayerId,
        accept: bool,
        now: Instant,
    ) -> Result<Option<ChallengeResponse>, LobbyError> {
        let Some(challenge) = self.challenges.get(&id).copied() else {
            return Ok(None);
        };
        if player != challenge.players[1] {
            return Ok(None);
        }
        // Consume before starting the match, preventing repeated reactions
        // from creating or overwriting games.
        self.challenges.remove(&id);
        if now.duration_since(challenge.created_at) >= CHALLENGE_TTL {
            return Ok(Some(ChallengeResponse::Expired));
        }
        if !accept {
            return Ok(Some(ChallengeResponse::Declined(challenge)));
        }
        self.start(challenge.players, challenge.channel, Opponent::Human)
            .map(ChallengeResponse::Accepted)
            .map(Some)
    }

    pub fn submit_bet(
        &mut self,
        player: PlayerId,
        channel: ChannelId,
        bet: i64,
        rng: &mut impl Rng,
    ) -> Result<BetResponse, LobbyError> {
        let key = *self.seats.get(&player).ok_or(LobbyError::NoGame)?;
        let active = self
            .matches
            .get_mut(&key)
            .expect("every seat references an active match");
        if active.snapshot.channel != channel {
            return Err(LobbyError::WrongChannel(active.snapshot.channel));
        }
        let side = if player == active.snapshot.players[0] {
            Player::One
        } else {
            Player::Two
        };
        if active.bets[side.index()].is_some() {
            return Err(LobbyError::AlreadyBet);
        }
        let bet = active.snapshot.game.validate_bet(side, bet)?;
        active.bets[side.index()] = Some(bet);
        if active.snapshot.opponent == Opponent::Bot {
            // The policy only sees the board and wallets, never the human bid.
            active.bets[1] = Some(strategy::choose_bet(&active.snapshot.game, rng));
        }
        let [Some(one), Some(two)] = active.bets else {
            return Ok(BetResponse::Waiting);
        };
        active
            .snapshot
            .game
            .play_round([i64::from(one), i64::from(two)])?;
        active.bets = [None; 2];
        let snapshot = active.snapshot.clone();
        if snapshot.game.outcome().is_some() {
            self.remove_match(key);
        }
        Ok(BetResponse::Round {
            snapshot,
            bets: [one, two],
        })
    }

    pub fn stop(
        &mut self,
        player: PlayerId,
        channel: ChannelId,
    ) -> Result<MatchSnapshot, LobbyError> {
        let snapshot = self.game_for(player).ok_or(LobbyError::NoGame)?;
        if snapshot.channel != channel {
            return Err(LobbyError::WrongChannel(snapshot.channel));
        }
        let key = snapshot.players[0];
        Ok(self.remove_match(key).expect("the player's match exists"))
    }

    fn remove_match(&mut self, key: PlayerId) -> Option<MatchSnapshot> {
        let active = self.matches.remove(&key)?;
        self.seats.remove(&active.snapshot.players[0]);
        if active.snapshot.opponent == Opponent::Human {
            self.seats.remove(&active.snapshot.players[1]);
        }
        Some(active.snapshot)
    }
}

#[cfg(test)]
mod tests {
    use rand::{SeedableRng, rngs::StdRng};

    use super::*;

    const ONE: PlayerId = PlayerId(1);
    const TWO: PlayerId = PlayerId(2);
    const THREE: PlayerId = PlayerId(3);
    const BOT: PlayerId = PlayerId(99);
    const CHANNEL: ChannelId = ChannelId(10);
    const REQUEST: ChallengeId = ChallengeId(100);

    fn human_game(lobby: &mut Lobby) {
        let now = Instant::now();
        lobby
            .add_challenge(REQUEST, [ONE, TWO], CHANNEL, now)
            .unwrap();
        assert!(matches!(
            lobby.respond_to_challenge(REQUEST, TWO, true, now).unwrap(),
            Some(ChallengeResponse::Accepted(_))
        ));
    }

    #[test]
    fn only_the_recipient_can_answer_and_only_once() {
        let mut lobby = Lobby::default();
        let now = Instant::now();
        lobby
            .add_challenge(REQUEST, [ONE, TWO], CHANNEL, now)
            .unwrap();
        assert_eq!(
            lobby.respond_to_challenge(REQUEST, ONE, true, now),
            Ok(None)
        );
        assert_eq!(
            lobby.respond_to_challenge(REQUEST, THREE, false, now),
            Ok(None)
        );
        assert!(matches!(
            lobby.respond_to_challenge(REQUEST, TWO, true, now).unwrap(),
            Some(ChallengeResponse::Accepted(_))
        ));
        assert_eq!(
            lobby.respond_to_challenge(REQUEST, TWO, true, now),
            Ok(None)
        );
        assert_eq!(lobby.game_for(ONE), lobby.game_for(TWO));
    }

    #[test]
    fn declined_and_expired_challenges_cannot_be_accepted() {
        for expired in [false, true] {
            let mut lobby = Lobby::default();
            let now = Instant::now();
            lobby
                .add_challenge(REQUEST, [ONE, TWO], CHANNEL, now)
                .unwrap();
            let response = lobby
                .respond_to_challenge(
                    REQUEST,
                    TWO,
                    expired,
                    now + if expired {
                        CHALLENGE_TTL
                    } else {
                        Duration::ZERO
                    },
                )
                .unwrap();
            assert!(matches!(
                response,
                Some(ChallengeResponse::Declined(_)) | Some(ChallengeResponse::Expired)
            ));
            assert_eq!(
                lobby.respond_to_challenge(REQUEST, TWO, true, now),
                Ok(None)
            );
            assert!(lobby.game_for(ONE).is_none());
        }
    }

    #[test]
    fn starting_another_game_invalidates_every_involving_challenge() {
        let mut lobby = Lobby::default();
        let now = Instant::now();
        lobby
            .add_challenge(REQUEST, [ONE, TWO], CHANNEL, now)
            .unwrap();
        lobby
            .add_challenge(ChallengeId(101), [THREE, ONE], CHANNEL, now)
            .unwrap();
        lobby.start_bot(ONE, BOT, CHANNEL).unwrap();
        assert!(lobby.challenges.is_empty());
        assert_eq!(
            lobby.respond_to_challenge(REQUEST, TWO, true, now),
            Ok(None)
        );
    }

    #[test]
    fn busy_players_and_duplicate_pending_challenges_are_rejected() {
        let mut lobby = Lobby::default();
        let now = Instant::now();
        assert_eq!(
            lobby.check_challenge([ONE, ONE], now),
            Err(LobbyError::SelfChallenge)
        );
        lobby
            .add_challenge(REQUEST, [ONE, TWO], CHANNEL, now)
            .unwrap();
        assert_eq!(
            lobby.check_challenge([ONE, THREE], now),
            Err(LobbyError::PendingChallenge)
        );
        lobby.start_bot(TWO, BOT, CHANNEL).unwrap();
        assert_eq!(
            lobby.add_challenge(REQUEST, [ONE, TWO], CHANNEL, now),
            Err(LobbyError::Busy(TWO))
        );
        assert_eq!(
            lobby.start_bot(TWO, BOT, CHANNEL),
            Err(LobbyError::Busy(TWO))
        );
    }

    #[test]
    fn expired_outgoing_challenges_are_pruned_before_new_requests() {
        let mut lobby = Lobby::default();
        let now = Instant::now();
        lobby
            .add_challenge(REQUEST, [ONE, TWO], CHANNEL, now)
            .unwrap();
        lobby
            .add_challenge(ChallengeId(101), [ONE, THREE], CHANNEL, now + CHALLENGE_TTL)
            .unwrap();
        assert_eq!(lobby.challenges.len(), 1);
    }

    #[test]
    fn bets_stay_secret_until_both_players_submit_and_cannot_be_replaced() {
        let mut lobby = Lobby::default();
        human_game(&mut lobby);
        let before = lobby.game_for(ONE).unwrap().clone();
        let mut rng = StdRng::seed_from_u64(1);
        assert_eq!(
            lobby.submit_bet(TWO, CHANNEL, 4, &mut rng),
            Ok(BetResponse::Waiting)
        );
        assert_eq!(lobby.game_for(ONE).unwrap(), &before);
        assert_eq!(
            lobby.submit_bet(TWO, CHANNEL, 5, &mut rng),
            Err(LobbyError::AlreadyBet)
        );
        let BetResponse::Round { snapshot, bets } =
            lobby.submit_bet(ONE, CHANNEL, 5, &mut rng).unwrap()
        else {
            panic!("both bets should resolve a round")
        };
        assert_eq!(bets, [5, 4]);
        assert_eq!(snapshot.game.coins(Player::One), 59);
        assert_eq!(snapshot.game.position(), -1);
        assert_eq!(
            lobby.submit_bet(ONE, CHANNEL, 0, &mut rng),
            Ok(BetResponse::Waiting)
        );
    }

    #[test]
    fn invalid_moves_and_wrong_channels_leave_pending_bets_unchanged() {
        let mut lobby = Lobby::default();
        human_game(&mut lobby);
        let mut rng = StdRng::seed_from_u64(1);
        assert_eq!(
            lobby.submit_bet(THREE, CHANNEL, 0, &mut rng),
            Err(LobbyError::NoGame)
        );
        assert_eq!(
            lobby.submit_bet(ONE, ChannelId(11), 5, &mut rng),
            Err(LobbyError::WrongChannel(CHANNEL))
        );
        assert_eq!(
            lobby.submit_bet(ONE, CHANNEL, -1, &mut rng),
            Err(LobbyError::Move(MoveError::InvalidBet { available: 64 }))
        );
        assert_eq!(
            lobby.submit_bet(TWO, CHANNEL, 65, &mut rng),
            Err(LobbyError::Move(MoveError::InvalidBet { available: 64 }))
        );
        assert_eq!(
            lobby.submit_bet(ONE, CHANNEL, 64, &mut rng),
            Ok(BetResponse::Waiting)
        );
        assert!(matches!(
            lobby.submit_bet(TWO, CHANNEL, 64, &mut rng),
            Ok(BetResponse::Round { .. })
        ));
        assert!(lobby.game_for(ONE).is_none());
        assert!(lobby.game_for(TWO).is_none());
    }

    #[test]
    fn stopping_either_player_removes_both_seats() {
        for player in [ONE, TWO] {
            let mut lobby = Lobby::default();
            human_game(&mut lobby);
            lobby.stop(player, CHANNEL).unwrap();
            assert!(lobby.matches.is_empty());
            assert!(lobby.seats.is_empty());
            assert_eq!(lobby.stop(player, CHANNEL), Err(LobbyError::NoGame));
        }
    }

    #[test]
    fn independent_bot_matches_share_no_seat_or_coin_state() {
        let mut lobby = Lobby::default();
        lobby.start_bot(ONE, BOT, CHANNEL).unwrap();
        lobby.start_bot(TWO, BOT, CHANNEL).unwrap();
        assert!(lobby.game_for(BOT).is_none());
        let mut rng = StdRng::seed_from_u64(2);
        assert!(matches!(
            lobby.submit_bet(ONE, CHANNEL, 1, &mut rng),
            Ok(BetResponse::Round { .. })
        ));
        assert_eq!(lobby.game_for(TWO).unwrap().game, Game::default());
        lobby.stop(ONE, CHANNEL).unwrap();
        assert!(lobby.game_for(TWO).is_some());
    }

    #[test]
    fn failed_announcements_release_seats_without_cancelling_a_newer_match() {
        let mut lobby = Lobby::default();
        let first = lobby.start_bot(ONE, BOT, CHANNEL).unwrap();
        assert!(lobby.cancel_start(&first));
        let second = lobby.start_bot(ONE, BOT, CHANNEL).unwrap();
        assert!(!lobby.cancel_start(&first));
        assert_eq!(lobby.game_for(ONE), Some(&second));
        lobby
            .submit_bet(ONE, CHANNEL, 1, &mut StdRng::seed_from_u64(1))
            .unwrap();
        assert!(!lobby.cancel_start(&second));
        assert!(lobby.game_for(ONE).is_some());
    }

    #[test]
    fn failed_announcements_do_not_discard_a_submitted_secret_bet() {
        let mut lobby = Lobby::default();
        human_game(&mut lobby);
        let snapshot = lobby.game_for(ONE).unwrap().clone();
        lobby
            .submit_bet(ONE, CHANNEL, 1, &mut StdRng::seed_from_u64(1))
            .unwrap();
        assert!(!lobby.cancel_start(&snapshot));
        assert!(lobby.game_for(TWO).is_some());
    }

    #[test]
    fn cancelling_a_challenge_does_not_affect_an_accepted_match() {
        let mut lobby = Lobby::default();
        let now = Instant::now();
        lobby
            .add_challenge(REQUEST, [ONE, TWO], CHANNEL, now)
            .unwrap();
        assert!(lobby.cancel_challenge(REQUEST));
        assert_eq!(
            lobby.respond_to_challenge(REQUEST, TWO, true, now),
            Ok(None)
        );
        human_game(&mut lobby);
        assert!(!lobby.cancel_challenge(REQUEST));
        assert!(lobby.game_for(ONE).is_some());
    }
}
