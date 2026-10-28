//! When does a corner fire?
//!
//! This is the part that decides whether the app feels solid or twitchy, so it
//! lives on its own and does nothing else. It holds no Win32 handles, touches
//! no config mutex, and runs no actions. It is fed a hot spot and the current
//! settings once per tick and answers "fire this corner" or "not yet".
//!
//! # The rules
//!
//! * **Dwell.** Resting in a corner for `dwell_ms` arms it. Without this,
//!   overshooting toward a window's Close button fires the corner every time.
//! * **Cooldown *and* leaving.** After firing, a spot stays disarmed until
//!   both the cooldown elapses and the cursor leaves. Both, not either:
//!   otherwise parking the mouse would repeat the action.
//! * **Vetoes.** A completed dwell can still be refused; see [`Veto`] for the
//!   two very different ways that plays out.
//!
//! A "spot" rather than a corner: in `EveryMonitor` mode every display has its
//! own top-left, and they must not be confused with each other. See
//! [`crate::geometry::Spot`].

use crate::config::{Config, Corner, Modifier, MonitorMode};
use crate::desktop::Desktop;
use crate::geometry::Spot;
use std::time::{Duration, Instant};
use traykit::{ldebug, linfo};

/// Settings lifted out of the config once per tick, so the hot path never
/// clones the `Action` strings.
#[derive(Clone, Copy, Debug)]
pub struct Tunables {
    pub enabled: bool,
    pub corner_size: i32,
    pub mode: MonitorMode,
    pub dwell: Duration,
    pub cooldown: Duration,
    pub modifier: Modifier,
    /// A key per corner, overriding `modifier` where set. The Mac window
    /// sets them, as System Settings does for the Dock's corners.
    pub corner_keys: [Modifier; 4],
    pub suppress_fullscreen: bool,
    pub suppress_dragging: bool,
    /// Whether any corner has something to do. When none has, the watcher
    /// can stop asking where the cursor is.
    pub any_assigned: bool,
}

impl Tunables {
    pub fn read(cfg: &Config) -> Self {
        Self {
            enabled: cfg.enabled,
            corner_size: cfg.corner_size,
            mode: cfg.monitor_mode,
            dwell: Duration::from_millis(cfg.dwell_ms as u64),
            cooldown: Duration::from_millis(cfg.cooldown_ms as u64),
            modifier: cfg.modifier,
            corner_keys: cfg.corner_keys,
            suppress_fullscreen: cfg.suppress_fullscreen,
            suppress_dragging: cfg.suppress_while_dragging,
            any_assigned: cfg.corners.iter().any(|a| !a.is_none()),
        }
    }

    /// The key that has to be held for `corner`.
    pub fn key_for(&self, corner: Corner) -> Modifier {
        match self.corner_keys[corner.index()] {
            Modifier::None => self.modifier,
            own => own,
        }
    }
}

/// What the caller should do about this tick.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tick {
    Nothing,
    /// This corner's action should run now.
    Fire(Corner),
}

#[derive(Clone, Copy, Debug)]
enum State {
    /// Cursor is not in any hot spot.
    Idle,
    /// Cursor is resting in a spot, waiting out the dwell.
    Dwelling { spot: Spot, since: Instant },
    /// A spot fired, or was vetoed. Stays disarmed until the cursor leaves
    /// *and* the cooldown elapses: both, not either.
    Fired { spot: Spot, at: Instant },
}

/// Why a dwell that has completed might still not fire.
///
/// The two variants differ in more than their reason, and that difference is
/// the whole point of naming them.
enum Veto {
    /// A required modifier is not held. Stay **armed**: pressing the key while
    /// already parked in the corner is meant to fire it.
    Waiting,
    /// The desktop says no. **Disarm**, so the corner cannot go off the
    /// instant the condition clears; see the note on each reason below.
    Blocked(&'static str),
}

/// The corner state machine. One per watcher thread.
pub struct Trigger {
    state: State,
}

impl Trigger {
    pub fn new() -> Self {
        Self { state: State::Idle }
    }

    /// Forget any dwell in progress. Used when the app is switched off, so
    /// switching it back on does not resume a half-finished dwell.
    pub fn reset(&mut self) {
        self.state = State::Idle;
    }

    /// Advance one tick. `at` is the hot spot the cursor is in, if any.
    pub fn update(&mut self, at: Option<Spot>, tun: &Tunables, desktop: &impl Desktop) -> Tick {
        let (next, tick) = advance(self.state, at, tun, desktop, Instant::now());
        self.state = next;
        tick
    }
}

/// The transitions, as a pure function of (state, input, now).
///
/// Separate from [`Trigger::update`] so tests can hand it a fabricated `now`
/// and drive every transition without waiting out real dwells.
fn advance(
    state: State,
    current: Option<Spot>,
    tun: &Tunables,
    desktop: &impl Desktop,
    now: Instant,
) -> (State, Tick) {
    let dwelling = |spot| (State::Dwelling { spot, since: now }, Tick::Nothing);
    let stay = (state, Tick::Nothing);

    match (state, current) {
        (State::Idle, None) => stay,
        (State::Idle, Some(spot)) => dwelling(spot),

        (State::Dwelling { .. }, None) => (State::Idle, Tick::Nothing),

        (State::Dwelling { spot, since }, Some(now_at)) => {
            // Compared by position, not by name: arriving at a *different*
            // display's top-left is a new dwell, not a continuation.
            if now_at != spot {
                return dwelling(now_at);
            }
            if now.duration_since(since) < tun.dwell {
                return stay;
            }
            let corner = spot.corner;
            let disarm = (State::Fired { spot, at: now }, Tick::Nothing);

            match veto(tun.key_for(corner), tun, desktop) {
                Some(Veto::Waiting) => {
                    ldebug!(
                        "{corner:?} dwell met but {:?} not held; staying armed",
                        tun.key_for(corner)
                    );
                    stay
                }
                Some(Veto::Blocked(why)) => {
                    linfo!("{corner:?} suppressed: {why}");
                    disarm
                }
                None => (State::Fired { spot, at: now }, Tick::Fire(corner)),
            }
        }

        // Still sitting in the spot that just fired: stay disarmed. Without
        // this the action would repeat every tick while the cursor rests.
        (State::Fired { spot, .. }, Some(now_at)) if now_at == spot => stay,

        // A different spot, or none at all: re-arm once the cooldown is up.
        (State::Fired { at, .. }, elsewhere) => {
            if now.duration_since(at) < tun.cooldown {
                stay
            } else {
                match elsewhere {
                    Some(spot) => dwelling(spot),
                    None => (State::Idle, Tick::Nothing),
                }
            }
        }
    }
}

fn veto(key: Modifier, tun: &Tunables, desktop: &impl Desktop) -> Option<Veto> {
    if !desktop.modifier_held(key) {
        return Some(Veto::Waiting);
    }
    // Dragging a window into a corner is how Windows Snap works, so a corner
    // must not also fire. Disarming rather than waiting matters: if we merely
    // waited, releasing the button to complete the snap would fire the corner
    // at that exact moment, which is the worst possible time.
    if tun.suppress_dragging && desktop.mouse_button_down() {
        return Some(Veto::Blocked("a mouse button is held (window drag / snap)"));
    }
    // Disarming rather than retrying every tick matters here too: re-querying
    // the shell notification state 33x/second while someone watches a
    // fullscreen film is exactly the waste this app exists to avoid.
    if tun.suppress_fullscreen && desktop.fullscreen_active() {
        return Some(Veto::Blocked("something is fullscreen"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A desktop with nothing in the way, which each test bends as needed.
    /// The real one cannot be used here: during `cargo test` no key is ever
    /// held and nothing is ever fullscreen, so every veto would be untestable.
    #[derive(Clone, Copy, Default)]
    struct Fake {
        modifier_held: bool,
        mouse_button_down: bool,
        fullscreen: bool,
    }

    impl Fake {
        fn clear() -> Self {
            Self {
                modifier_held: true,
                ..Default::default()
            }
        }
    }

    impl Desktop for Fake {
        fn modifier_held(&self, _m: Modifier) -> bool {
            self.modifier_held
        }
        fn mouse_button_down(&self) -> bool {
            self.mouse_button_down
        }
        fn fullscreen_active(&self) -> bool {
            self.fullscreen
        }
    }

    fn tun() -> Tunables {
        Tunables {
            enabled: true,
            corner_size: 8,
            mode: MonitorMode::OuterCorners,
            dwell: Duration::from_millis(250),
            cooldown: Duration::from_millis(700),
            modifier: Modifier::None,
            corner_keys: [Modifier::None; 4],
            suppress_fullscreen: false,
            suppress_dragging: false,
            any_assigned: true,
        }
    }

    /// A hot spot on the notional first display.
    fn spot(corner: Corner) -> Spot {
        Spot {
            corner,
            anchor: (0, 0),
        }
    }

    /// The same corner on a second display to the right. Distinct from
    /// `spot(corner)` even though it carries the same `Corner`.
    fn spot_on_second(corner: Corner) -> Spot {
        Spot {
            corner,
            anchor: (1920, 0),
        }
    }

    fn ago(ms: u64) -> Instant {
        Instant::now() - Duration::from_millis(ms)
    }

    /// Drive one transition with an unobstructed desktop.
    fn step(state: State, at: Option<Spot>) -> (State, Tick) {
        advance(state, at, &tun(), &Fake::clear(), Instant::now())
    }

    fn dwelling_since(corner: Corner, ms: u64) -> State {
        State::Dwelling {
            spot: spot(corner),
            since: ago(ms),
        }
    }

    fn fired_at(corner: Corner, ms: u64) -> State {
        State::Fired {
            spot: spot(corner),
            at: ago(ms),
        }
    }

    // -- Dwell -------------------------------------------------------------

    #[test]
    fn entering_a_corner_starts_the_dwell() {
        match step(State::Idle, Some(spot(Corner::TopLeft))) {
            (State::Dwelling { spot: s, .. }, Tick::Nothing) => {
                assert_eq!(s, spot(Corner::TopLeft))
            }
            other => panic!("expected a dwell, got {other:?}"),
        }
    }

    #[test]
    fn idle_stays_idle_outside_any_corner() {
        assert!(matches!(
            step(State::Idle, None),
            (State::Idle, Tick::Nothing)
        ));
    }

    #[test]
    fn dwell_not_yet_satisfied_keeps_waiting() {
        let (state, tick) = step(
            dwelling_since(Corner::TopLeft, 100),
            Some(spot(Corner::TopLeft)),
        );
        assert!(matches!(state, State::Dwelling { .. }));
        assert_eq!(tick, Tick::Nothing);
    }

    #[test]
    fn dwell_satisfied_fires() {
        let (state, tick) = step(
            dwelling_since(Corner::TopLeft, 300),
            Some(spot(Corner::TopLeft)),
        );
        assert!(matches!(state, State::Fired { .. }));
        assert_eq!(tick, Tick::Fire(Corner::TopLeft));
    }

    #[test]
    fn leaving_mid_dwell_resets() {
        assert!(matches!(
            step(dwelling_since(Corner::TopLeft, 100), None),
            (State::Idle, Tick::Nothing)
        ));
    }

    #[test]
    fn sliding_to_another_corner_restarts_the_dwell() {
        // Would have fired in 10ms had we stayed; the new corner starts over.
        let (state, tick) = step(
            dwelling_since(Corner::TopLeft, 240),
            Some(spot(Corner::TopRight)),
        );
        match state {
            State::Dwelling { spot: s, since } => {
                assert_eq!(s.corner, Corner::TopRight);
                assert!(since.elapsed() < Duration::from_millis(50));
            }
            other => panic!("expected a fresh dwell, got {other:?}"),
        }
        assert_eq!(tick, Tick::Nothing);
    }

    // -- Cooldown ----------------------------------------------------------

    #[test]
    fn resting_in_a_fired_corner_does_not_refire() {
        // The regression that would make an action repeat 33 times a second.
        let (state, tick) = step(
            fired_at(Corner::TopLeft, 5_000),
            Some(spot(Corner::TopLeft)),
        );
        assert!(matches!(state, State::Fired { .. }));
        assert_eq!(tick, Tick::Nothing);
    }

    #[test]
    fn leaving_before_the_cooldown_stays_disarmed() {
        assert!(matches!(
            step(fired_at(Corner::TopLeft, 200), None),
            (State::Fired { .. }, Tick::Nothing)
        ));
    }

    #[test]
    fn leaving_after_the_cooldown_rearms() {
        assert!(matches!(
            step(fired_at(Corner::TopLeft, 900), None),
            (State::Idle, Tick::Nothing)
        ));
    }

    // -- Same corner, different display (EveryMonitor mode) ----------------

    #[test]
    fn a_second_displays_top_left_is_not_the_one_that_just_fired() {
        // Both spots are `TopLeft`. Keyed on the corner name alone, the second
        // display's corner stayed disarmed until the cursor left it, so a
        // quick flick between screens silently did nothing.
        let inside_cooldown = step(
            fired_at(Corner::TopLeft, 100),
            Some(spot_on_second(Corner::TopLeft)),
        );
        assert!(
            matches!(inside_cooldown, (State::Fired { .. }, Tick::Nothing)),
            "the cooldown belongs to the spot that fired, and is not yet up"
        );

        // Once it is up, the other display's corner arms normally.
        match step(
            fired_at(Corner::TopLeft, 900),
            Some(spot_on_second(Corner::TopLeft)),
        ) {
            (State::Dwelling { spot: s, .. }, Tick::Nothing) => {
                assert_eq!(s, spot_on_second(Corner::TopLeft))
            }
            other => panic!("expected a fresh dwell, got {other:?}"),
        }
    }

    #[test]
    fn crossing_to_the_same_corner_on_another_display_restarts_the_dwell() {
        let (state, _) = step(
            dwelling_since(Corner::TopLeft, 240), // would have fired in 10ms
            Some(spot_on_second(Corner::TopLeft)),
        );
        match state {
            State::Dwelling { spot: s, since } => {
                assert_eq!(s, spot_on_second(Corner::TopLeft));
                assert!(since.elapsed() < Duration::from_millis(50));
            }
            other => panic!("expected a fresh dwell, got {other:?}"),
        }
    }

    // -- Vetoes ------------------------------------------------------------

    /// Drive a satisfied dwell against a given desktop and settings.
    fn satisfied_dwell(tun: &Tunables, desktop: Fake) -> (State, Tick) {
        advance(
            dwelling_since(Corner::TopLeft, 400),
            Some(spot(Corner::TopLeft)),
            tun,
            &desktop,
            Instant::now(),
        )
    }

    #[test]
    fn a_required_modifier_that_is_not_held_keeps_the_corner_armed() {
        // Armed, not disarmed: pressing the key while already parked in the
        // corner is meant to fire it.
        let mut t = tun();
        t.modifier = Modifier::Ctrl;
        let held_down = Fake {
            modifier_held: false,
            ..Default::default()
        };
        let (state, tick) = satisfied_dwell(&t, held_down);
        assert!(matches!(state, State::Dwelling { .. }));
        assert_eq!(tick, Tick::Nothing);
    }

    #[test]
    fn a_required_modifier_that_is_held_fires() {
        let mut t = tun();
        t.modifier = Modifier::Ctrl;
        let (_, tick) = satisfied_dwell(&t, Fake::clear());
        assert_eq!(tick, Tick::Fire(Corner::TopLeft));
    }

    #[test]
    fn a_corners_own_key_overrides_the_shared_one() {
        let mut t = tun();
        t.corner_keys[Corner::TopLeft.index()] = Modifier::Win;
        assert_eq!(t.key_for(Corner::TopLeft), Modifier::Win);
        assert_eq!(t.key_for(Corner::TopRight), Modifier::None);
        let not_held = Fake {
            modifier_held: false,
            ..Default::default()
        };
        let (state, tick) = satisfied_dwell(&t, not_held);
        assert!(matches!(state, State::Dwelling { .. }));
        assert_eq!(tick, Tick::Nothing);
    }

    #[test]
    fn a_held_mouse_button_disarms_the_corner() {
        // Disarmed, not merely delayed: otherwise releasing the button to
        // complete a Snap would fire the corner at that exact moment.
        let mut t = tun();
        t.suppress_dragging = true;
        let dragging = Fake {
            mouse_button_down: true,
            ..Fake::clear()
        };
        let (state, tick) = satisfied_dwell(&t, dragging);
        assert!(matches!(state, State::Fired { .. }));
        assert_eq!(tick, Tick::Nothing);
    }

    #[test]
    fn a_held_mouse_button_is_ignored_when_the_rule_is_off() {
        let dragging = Fake {
            mouse_button_down: true,
            ..Fake::clear()
        };
        let (_, tick) = satisfied_dwell(&tun(), dragging);
        assert_eq!(tick, Tick::Fire(Corner::TopLeft));
    }

    #[test]
    fn fullscreen_disarms_the_corner() {
        let mut t = tun();
        t.suppress_fullscreen = true;
        let gaming = Fake {
            fullscreen: true,
            ..Fake::clear()
        };
        let (state, tick) = satisfied_dwell(&t, gaming);
        assert!(matches!(state, State::Fired { .. }));
        assert_eq!(tick, Tick::Nothing);
    }

    #[test]
    fn fullscreen_is_ignored_when_the_rule_is_off() {
        let gaming = Fake {
            fullscreen: true,
            ..Fake::clear()
        };
        let (_, tick) = satisfied_dwell(&tun(), gaming);
        assert_eq!(tick, Tick::Fire(Corner::TopLeft));
    }

    #[test]
    fn a_disarming_veto_needs_the_cursor_to_leave_before_it_can_fire() {
        // The rule that makes suppression stick: once vetoed, waiting in place
        // is not enough even after the condition clears and the cooldown ends.
        let mut t = tun();
        t.suppress_dragging = true;
        let (disarmed, _) = satisfied_dwell(
            &t,
            Fake {
                mouse_button_down: true,
                ..Fake::clear()
            },
        );
        // Button released, cooldown long past, still parked in the corner.
        let (state, tick) = advance(
            disarmed,
            Some(spot(Corner::TopLeft)),
            &t,
            &Fake::clear(),
            Instant::now() + Duration::from_secs(5),
        );
        assert!(matches!(state, State::Fired { .. }));
        assert_eq!(tick, Tick::Nothing, "must leave the corner and come back");
    }
}
