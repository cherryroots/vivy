//! The rules of one game, with no Discord, network or clock in it: the caller passes the
//! time in (`now`, in milliseconds), so the tests can play a whole game in a few lines.
//!
//! A game goes Lobby → Loading → (Question → Reveal) × rounds → Done, and from Done (or
//! the Lobby after a failed load) back to Loading when someone presses Start again.

use std::collections::HashMap;

use serde::Serialize;

/// How long the answer stays on screen before the next question.
pub const REVEAL_MS: u64 = 6_000;
/// Points for a right answer, plus up to this much again for answering fast.
const POINTS: u32 = 100;

/// Someone who can be an answer: a quote's author, or one of the wrong choices.
#[derive(Debug, Clone, PartialEq)]
pub struct Person {
    pub id: u64,
    pub name: String,
    /// An avatar URL the page can load, or `None` to show the first letter instead.
    pub avatar: Option<String>,
}

/// A message to guess the author of.
#[derive(Debug, Clone, PartialEq)]
pub struct Quote {
    pub author: u64,
    pub text: String,
    /// When it was posted, as shown under the quote ("March 2025").
    pub posted: String,
    /// A link to the message, shown with the answer.
    pub link: String,
}

/// One question: the quote, and the people to choose from (the author among them).
#[derive(Debug, Clone)]
pub struct Round {
    pub quote: Quote,
    pub choices: Vec<Person>,
    /// What each player picked, and how many milliseconds they took.
    picks: HashMap<u64, (u64, u64)>,
    /// What each player got for this round, filled in when it's revealed.
    gained: HashMap<u64, u32>,
}

impl Round {
    pub fn new(quote: Quote, choices: Vec<Person>) -> Self {
        Round {
            quote,
            choices,
            picks: HashMap::new(),
            gained: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Player {
    pub person: Person,
    pub score: u32,
    /// Whether their window is open. Players who leave keep their score, so closing the
    /// window by mistake and coming back loses nothing.
    pub connected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phase {
    Lobby,
    Loading,
    /// Round `index` is being answered; it ends at `ends_at`.
    Question {
        index: usize,
        started_at: u64,
        ends_at: u64,
    },
    /// Round `index`'s answer is shown until `ends_at`.
    Reveal {
        index: usize,
        ends_at: u64,
    },
    Done,
}

pub struct Game {
    pub phase: Phase,
    /// In the order they joined.
    pub players: Vec<Player>,
    rounds: Vec<Round>,
    answer_ms: u64,
    /// Why the last load failed, shown in the lobby.
    error: Option<String>,
}

impl Game {
    pub fn new(answer_seconds: u64) -> Self {
        Game {
            phase: Phase::Lobby,
            players: Vec::new(),
            rounds: Vec::new(),
            answer_ms: answer_seconds.max(5) * 1000,
            error: None,
        }
    }

    /// Adds a player, or marks one who left as back.
    pub fn join(&mut self, person: Person) {
        match self.players.iter_mut().find(|p| p.person.id == person.id) {
            Some(player) => {
                player.person = person;
                player.connected = true;
            }
            None => self.players.push(Player {
                person,
                score: 0,
                connected: true,
            }),
        }
    }

    pub fn leave(&mut self, id: u64) {
        if let Some(player) = self.players.iter_mut().find(|p| p.person.id == id) {
            player.connected = false;
        }
    }

    pub fn anyone_here(&self) -> bool {
        self.players.iter().any(|p| p.connected)
    }

    /// Starts loading quotes, if no game is running. Returns whether it started.
    pub fn start_loading(&mut self) -> bool {
        if !matches!(self.phase, Phase::Lobby | Phase::Done) {
            return false;
        }
        self.phase = Phase::Loading;
        self.error = None;
        true
    }

    /// Loading failed: back to the lobby, with the reason shown there.
    pub fn load_failed(&mut self, reason: String) {
        self.phase = Phase::Lobby;
        self.error = Some(reason);
    }

    /// The quotes are ready: scores go back to 0 and the first question starts.
    pub fn begin(&mut self, rounds: Vec<Round>, now: u64) {
        // People who left during an earlier game are dropped; their scores are over.
        self.players.retain(|p| p.connected);
        for player in &mut self.players {
            player.score = 0;
        }
        self.rounds = rounds;
        self.ask(0, now);
    }

    fn ask(&mut self, index: usize, now: u64) {
        self.phase = if index < self.rounds.len() {
            Phase::Question {
                index,
                started_at: now,
                ends_at: now + self.answer_ms,
            }
        } else {
            Phase::Done
        };
    }

    /// Records a player's answer. Only the first pick counts, and only one of the choices.
    /// Returns true when every player who is here has answered, so the round can end early.
    pub fn pick(&mut self, player: u64, choice: u64, now: u64) -> bool {
        let Phase::Question {
            index, started_at, ..
        } = self.phase
        else {
            return false;
        };
        if !self.players.iter().any(|p| p.person.id == player) {
            return false;
        }
        let round = &mut self.rounds[index];
        if round.choices.iter().any(|c| c.id == choice) {
            round
                .picks
                .entry(player)
                .or_insert((choice, now.saturating_sub(started_at)));
        }
        self.players
            .iter()
            .filter(|p| p.connected)
            .all(|p| round.picks.contains_key(&p.person.id))
    }

    /// Ends the question: scores it and shows the answer.
    pub fn reveal(&mut self, now: u64) {
        let Phase::Question { index, .. } = self.phase else {
            return;
        };
        let round = &mut self.rounds[index];
        for (&player, &(choice, took)) in &round.picks {
            if choice != round.quote.author {
                continue;
            }
            let gained = points(took, self.answer_ms);
            round.gained.insert(player, gained);
            if let Some(p) = self.players.iter_mut().find(|p| p.person.id == player) {
                p.score += gained;
            }
        }
        self.phase = Phase::Reveal {
            index,
            ends_at: now + REVEAL_MS,
        };
    }

    /// When the current phase ends by itself, if it does.
    pub fn deadline(&self) -> Option<u64> {
        match self.phase {
            Phase::Question { ends_at, .. } | Phase::Reveal { ends_at, .. } => Some(ends_at),
            _ => None,
        }
    }

    /// Moves on when the deadline has passed: a question to its answer, an answer to the
    /// next question (or the end).
    pub fn tick(&mut self, now: u64) {
        match self.phase {
            Phase::Question { ends_at, .. } if now >= ends_at => self.reveal(now),
            Phase::Reveal { index, ends_at } if now >= ends_at => self.ask(index + 1, now),
            _ => {}
        }
    }

    /// The players by score, best first, for the final board.
    pub fn standings(&self) -> Vec<&Player> {
        let mut players: Vec<&Player> = self.players.iter().collect();
        players.sort_by_key(|p| std::cmp::Reverse(p.score));
        players
    }

    /// What every window shows, as JSON. IDs are strings: JavaScript numbers can't hold a
    /// Discord ID exactly.
    pub fn view(&self, now: u64) -> View {
        let (phase, round) = match self.phase {
            Phase::Lobby => ("lobby", None),
            Phase::Loading => ("loading", None),
            Phase::Done => ("done", None),
            Phase::Question { index, ends_at, .. } => (
                "question",
                Some(self.round_view(index, ends_at, now, false)),
            ),
            Phase::Reveal { index, ends_at } => {
                ("reveal", Some(self.round_view(index, ends_at, now, true)))
            }
        };
        let answered = |id: u64| match self.phase {
            Phase::Question { index, .. } => self.rounds[index].picks.contains_key(&id),
            _ => false,
        };
        View {
            phase,
            error: self.error.clone(),
            players: self
                .players
                .iter()
                .map(|p| PlayerView {
                    id: p.person.id.to_string(),
                    name: p.person.name.clone(),
                    avatar: p.person.avatar.clone(),
                    score: p.score,
                    connected: p.connected,
                    answered: answered(p.person.id),
                })
                .collect(),
            round,
        }
    }

    fn round_view(&self, index: usize, ends_at: u64, now: u64, revealed: bool) -> RoundView {
        let round = &self.rounds[index];
        let ids = |map: &HashMap<u64, (u64, u64)>| {
            map.iter()
                .map(|(player, (choice, _))| (player.to_string(), choice.to_string()))
                .collect()
        };
        RoundView {
            number: index + 1,
            total: self.rounds.len(),
            text: round.quote.text.clone(),
            posted: round.quote.posted.clone(),
            choices: round
                .choices
                .iter()
                .map(|c| PersonView {
                    id: c.id.to_string(),
                    name: c.name.clone(),
                    avatar: c.avatar.clone(),
                })
                .collect(),
            ends_in_ms: ends_at.saturating_sub(now),
            answer_ms: self.answer_ms,
            // What people picked and the answer stay hidden until the reveal, so nobody can
            // read them from the page while the question is open.
            answer: revealed.then(|| round.quote.author.to_string()),
            link: revealed.then(|| round.quote.link.clone()),
            picks: if revealed {
                ids(&round.picks)
            } else {
                HashMap::new()
            },
            gained: round
                .gained
                .iter()
                .map(|(player, points)| (player.to_string(), *points))
                .collect(),
        }
    }
}

/// Points for a right answer that took `took` of `answer_ms`: 100, plus up to 100 more for
/// answering fast.
fn points(took: u64, answer_ms: u64) -> u32 {
    let left = answer_ms.saturating_sub(took);
    POINTS + (u64::from(POINTS) * left / answer_ms.max(1)) as u32
}

#[derive(Debug, Serialize)]
pub struct View {
    pub phase: &'static str,
    pub error: Option<String>,
    pub players: Vec<PlayerView>,
    pub round: Option<RoundView>,
}

#[derive(Debug, Serialize)]
pub struct PlayerView {
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
    pub score: u32,
    pub connected: bool,
    pub answered: bool,
}

#[derive(Debug, Serialize)]
pub struct PersonView {
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RoundView {
    pub number: usize,
    pub total: usize,
    pub text: String,
    pub posted: String,
    pub choices: Vec<PersonView>,
    pub ends_in_ms: u64,
    pub answer_ms: u64,
    pub answer: Option<String>,
    pub link: Option<String>,
    /// Player ID → the person they picked, after the reveal.
    pub picks: HashMap<String, String>,
    /// Player ID → points they got this round, after the reveal.
    pub gained: HashMap<String, u32>,
}

/// Picks up to `count` rounds from `quotes`, with up to 4 choices each: the author and
/// others from `people`. `shuffle` puts a list in random order (the tests pass one that
/// doesn't, so the result is known).
///
/// Nobody is the answer more than twice while someone else still could be, so one chatty
/// person doesn't make every answer the same.
pub fn make_rounds(
    quotes: Vec<Quote>,
    people: &[Person],
    count: usize,
    mut shuffle: impl FnMut(&mut Vec<Person>),
) -> Vec<Round> {
    let mut used: HashMap<u64, usize> = HashMap::new();
    let mut picked = Vec::new();
    let mut later = Vec::new();
    for quote in quotes {
        let times = used.entry(quote.author).or_default();
        if *times >= 2 {
            later.push(quote);
            continue;
        }
        *times += 1;
        picked.push(quote);
    }
    // Not enough variety: fill up with the rest.
    picked.extend(later);
    picked.truncate(count);

    picked
        .into_iter()
        .filter_map(|quote| {
            let author = people.iter().find(|p| p.id == quote.author)?.clone();
            let mut others: Vec<Person> = people
                .iter()
                .filter(|p| p.id != author.id)
                .cloned()
                .collect();
            shuffle(&mut others);
            others.truncate(3);
            if others.is_empty() {
                return None;
            }
            let mut choices = others;
            choices.push(author);
            shuffle(&mut choices);
            Some(Round::new(quote, choices))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(id: u64) -> Person {
        Person {
            id,
            name: format!("person {id}"),
            avatar: None,
        }
    }

    fn quote(author: u64) -> Quote {
        Quote {
            author,
            text: format!("something {author} said"),
            posted: "March 2025".into(),
            link: "https://discord.com/channels/1/2/3".into(),
        }
    }

    fn no_shuffle(_: &mut Vec<Person>) {}

    #[test]
    fn rounds_have_the_author_and_up_to_three_others() {
        let people: Vec<Person> = (1..=6).map(person).collect();
        let rounds = make_rounds(vec![quote(1), quote(2)], &people, 10, no_shuffle);
        assert_eq!(rounds.len(), 2);
        for round in &rounds {
            assert_eq!(round.choices.len(), 4);
            assert!(round.choices.iter().any(|c| c.id == round.quote.author));
        }
    }

    #[test]
    fn nobody_is_the_answer_more_than_twice_while_others_could_be() {
        let people: Vec<Person> = (1..=3).map(person).collect();
        let quotes = vec![quote(1), quote(1), quote(1), quote(1), quote(2), quote(3)];
        let rounds = make_rounds(quotes, &people, 4, no_shuffle);
        let authors: Vec<u64> = rounds.iter().map(|r| r.quote.author).collect();
        assert_eq!(authors, vec![1, 1, 2, 3]);
    }

    #[test]
    fn a_quote_needs_someone_else_to_choose() {
        let rounds = make_rounds(vec![quote(1)], &[person(1)], 10, no_shuffle);
        assert!(rounds.is_empty());
    }

    #[test]
    fn fast_right_answers_score_more() {
        assert_eq!(points(0, 20_000), 200);
        assert_eq!(points(10_000, 20_000), 150);
        assert_eq!(points(20_000, 20_000), 100);
        assert_eq!(points(30_000, 20_000), 100);
    }

    #[test]
    fn a_whole_game() {
        let people: Vec<Person> = (1..=4).map(person).collect();
        let mut game = Game::new(20);
        game.join(person(10));
        game.join(person(11));
        assert!(game.start_loading());
        assert!(!game.start_loading(), "can't start twice");
        game.begin(
            make_rounds(vec![quote(1), quote(2)], &people, 2, no_shuffle),
            0,
        );

        // Round 1: player 10 is right at once, 11 is wrong. Both answered, so it can end.
        assert!(!game.pick(10, 1, 0));
        assert!(!game.pick(10, 2, 0), "only the first pick counts");
        assert!(game.pick(11, 3, 5_000));
        // The answer isn't sent while the question is open.
        assert_eq!(game.view(0).round.unwrap().answer, None);
        game.reveal(5_000);
        let view = game.view(5_000);
        assert_eq!(view.phase, "reveal");
        assert_eq!(view.round.unwrap().answer.as_deref(), Some("1"));
        assert_eq!(game.players[0].score, 200);
        assert_eq!(game.players[1].score, 0);

        // The reveal ends by itself, then round 2 times out with no answers.
        game.tick(5_000 + REVEAL_MS);
        assert!(matches!(game.phase, Phase::Question { index: 1, .. }));
        let deadline = game.deadline().unwrap();
        game.tick(deadline);
        game.tick(deadline + REVEAL_MS);
        assert_eq!(game.phase, Phase::Done);
        assert_eq!(game.standings()[0].person.id, 10);

        // Playing again starts the scores over.
        assert!(game.start_loading());
        game.begin(make_rounds(vec![quote(1)], &people, 1, no_shuffle), 0);
        assert!(game.players.iter().all(|p| p.score == 0));
    }

    #[test]
    fn someone_who_left_doesnt_hold_up_the_round() {
        let people: Vec<Person> = (1..=4).map(person).collect();
        let mut game = Game::new(20);
        game.join(person(10));
        game.join(person(11));
        game.start_loading();
        game.begin(make_rounds(vec![quote(1)], &people, 1, no_shuffle), 0);
        game.leave(11);
        assert!(game.pick(10, 1, 100));
    }

    #[test]
    fn coming_back_keeps_the_score() {
        let mut game = Game::new(20);
        game.join(person(10));
        game.players[0].score = 300;
        game.leave(10);
        assert!(!game.anyone_here());
        game.join(person(10));
        assert_eq!(game.players.len(), 1);
        assert_eq!(game.players[0].score, 300);
    }
}
