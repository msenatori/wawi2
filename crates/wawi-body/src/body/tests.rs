//! The body without a screen: made-up worlds and made-up seconds.

use super::world::*;
use super::*;

const SIZE: (f64, f64) = (124.0, 110.0);
/// A 1440×900 screen with an 80-point Dock and a 25-point menu bar.
const FLOOR: f64 = 80.0;
const SCREEN: Screen = Screen {
    frame: (0.0, 0.0, 1440.0, 900.0),
    visible: (0.0, FLOOR, 1440.0, 795.0),
};

fn world(windows: Vec<Win>) -> Snapshot {
    Snapshot {
        version: 1,
        screens: vec![SCREEN],
        windows,
    }
}

fn win(id: u32, x: f64, top: f64, w: f64, h: f64) -> Win {
    Win {
        id,
        rect: (x, top - h, w, h),
    }
}

/// Free, awake, idle: she may wander.
const FREE: Ctx = Ctx {
    calm: false,
    free: true,
    asleep: false,
    still: false,
    cursor: None,
};

/// Runs `secs` at `fps` from `t`, returning every output.
fn run(w: &mut Walker, world: &Snapshot, t: &mut f64, secs: f64, fps: f64, ctx: Ctx) -> Vec<Out> {
    let mut outs = Vec::new();
    let steps = (secs * fps).round() as usize;
    for _ in 0..steps {
        *t += 1.0 / fps;
        outs.push(w.tick(*t, world, ctx));
    }
    outs
}

/// A walker that never wanders on its own (for the physics tests).
fn still_params() -> Params {
    Params {
        wander_every: (1e9, 1e9),
        ..Params::default()
    }
}

fn settled(at: (f64, f64), world: &Snapshot, params: Params) -> (Walker, f64) {
    let mut w = Walker::new(params, SIZE, at, 7);
    let mut t = 0.0;
    w.tick(t, world, FREE);
    t += 0.01;
    w.tick(t, world, FREE);
    (w, t)
}

/// Seconds from letting go until her paws touch, at `fps`.
fn fall_time(height: f64, fps: f64) -> f64 {
    let world = world(vec![]);
    let (mut w, mut t) = settled((700.0, FLOOR), &world, still_params());
    w.drop_from(height);
    let start = t;
    loop {
        t += 1.0 / fps;
        let out = w.tick(t, &world, FREE);
        if out.touchdown.is_some() {
            return t - start;
        }
        assert!(t - start < 10.0, "never landed");
    }
}

#[test]
fn she_falls_with_wawi_1s_gravity() {
    // h = g t² / 2 with g = 1500: 300 points take 0.63 s.
    let t = fall_time(300.0, 60.0);
    assert!((t - (2.0 * 300.0 / 1500.0f64).sqrt()).abs() < 0.04, "{t}");
}

#[test]
fn the_fall_feels_the_same_at_any_frame_rate() {
    let a = fall_time(400.0, 30.0);
    let b = fall_time(400.0, 120.0);
    assert!((a - b).abs() < 1.0 / 30.0 + 0.01, "{a} vs {b}");
}

#[test]
fn she_never_falls_faster_than_terminal_speed() {
    let world = world(vec![]);
    let (mut w, mut t) = settled((700.0, FLOOR), &world, still_params());
    w.drop_from(700.0);
    let mut fastest: f64 = 0.0;
    for _ in 0..120 {
        t += 1.0 / 60.0;
        w.tick(t, &world, FREE);
        fastest = fastest.max(-w.body().vy);
    }
    assert!((fastest - 950.0).abs() < 1e-9, "{fastest}");
}

#[test]
fn she_lands_on_the_floor_above_the_dock_and_squashes() {
    let world = world(vec![]);
    let (mut w, mut t) = settled((700.0, FLOOR), &world, still_params());
    w.drop_from(200.0);
    let outs = run(&mut w, &world, &mut t, 1.0, 60.0, FREE);
    let touch = outs.iter().position(|o| o.touchdown.is_some()).unwrap();
    assert_eq!(outs[touch].touchdown, Some(Touch::Soft));
    assert_eq!(outs[touch].ground.1, FLOOR);
    assert_eq!(outs[touch].loco, Some(Loco::Land));
    // The squash lasts Wawi 1's 0.35 s, then she stands.
    let stands = outs[touch..]
        .iter()
        .position(|o| o.mode == Mode::Quieta)
        .unwrap();
    assert!((stands as f64 / 60.0 - 0.35).abs() < 0.03, "{stands}");
    assert!(outs[touch + stands].settled);
}

#[test]
fn a_window_top_catches_her() {
    let world = world(vec![win(1, 500.0, 500.0, 500.0, 300.0)]);
    let (mut w, mut t) = settled((700.0, 700.0), &world, still_params());
    // Left in mid-air at start: she is put straight on the window.
    assert_eq!(w.ground().1, 500.0);
    assert_eq!(w.mode(), Mode::Posada);
    w.drop_from(150.0);
    let outs = run(&mut w, &world, &mut t, 1.5, 60.0, FREE);
    assert_eq!(outs.last().unwrap().ground.1, 500.0);
    assert!(outs.last().unwrap().perched);
}

#[test]
fn she_falls_when_her_window_moves_or_closes() {
    let here = world(vec![win(1, 500.0, 500.0, 500.0, 300.0)]);
    let (mut w, mut t) = settled((700.0, 500.0), &here, still_params());
    assert_eq!(w.mode(), Mode::Posada);
    // Dragged up by 40 points: no longer under her paws.
    let moved = Snapshot {
        version: 2,
        ..world(vec![win(1, 500.0, 540.0, 500.0, 300.0)])
    };
    let outs = run(&mut w, &moved, &mut t, 0.1, 60.0, FREE);
    assert!(outs.iter().any(|o| o.loco == Some(Loco::Fall)));
    // Closed: down to the floor.
    let gone = Snapshot {
        version: 3,
        ..world(vec![])
    };
    let outs = run(&mut w, &gone, &mut t, 2.0, 60.0, FREE);
    assert!(outs.iter().any(|o| o.touchdown.is_some()));
    assert_eq!(w.ground().1, FLOOR);
}

#[test]
fn a_window_in_front_hides_the_edge_behind_it() {
    let back = win(2, 100.0, 500.0, 800.0, 300.0);
    let front = win(1, 300.0, 560.0, 300.0, 400.0);
    let ls = ledges(&[SCREEN], &[front, back], SIZE);
    let tops: Vec<&Ledge> = ls.iter().filter(|l| l.window == Some(2)).collect();
    assert_eq!(tops.len(), 2, "{ls:?}");
    assert!(
        tops.iter()
            .all(|l| l.x1 <= 300.0 - SIZE.0 / 2.0 || l.x0 >= 600.0 + SIZE.0 / 2.0)
    );
}

#[test]
fn the_window_list_keeps_only_furniture() {
    let raw = |id, layer, pid, owner: &str, alpha, w, h| RawWindow {
        id,
        layer,
        pid,
        owner: owner.into(),
        alpha,
        bounds: (10.0, 100.0, w, h),
    };
    let list = vec![
        raw(1, 0, 50, "Safari", 1.0, 800.0, 600.0),
        raw(2, 25, 50, "Safari", 1.0, 800.0, 600.0),
        raw(3, 0, 99, "wawi2", 1.0, 800.0, 600.0),
        raw(4, 0, 51, "Dock", 1.0, 800.0, 600.0),
        raw(5, 0, 52, "Notes", 0.1, 800.0, 600.0),
        raw(6, 0, 53, "Notes", 1.0, 100.0, 40.0),
    ];
    let wins = parse_windows(&list, 99, 900.0);
    assert_eq!(wins.len(), 1);
    // Quartz's top-left y-down flipped to AppKit's bottom-left y-up.
    assert_eq!(wins[0].rect, (10.0, 900.0 - 100.0 - 600.0, 800.0, 600.0));
}

#[test]
fn a_voice_turn_stops_her_but_a_fall_finishes() {
    let world = world(vec![]);
    let (mut w, mut t) = settled((300.0, FLOOR), &world, still_params());
    w.wander_soon(t);
    run(&mut w, &world, &mut t, 0.5, 30.0, FREE);
    assert_eq!(w.mode(), Mode::Caminando);
    let calm = Ctx {
        calm: true,
        free: false,
        ..FREE
    };
    run(&mut w, &world, &mut t, 0.1, 30.0, calm);
    let x = w.ground().0;
    run(&mut w, &world, &mut t, 2.0, 30.0, calm);
    assert_eq!(w.mode(), Mode::Quieta);
    assert_eq!(w.ground().0, x);
    // Falling, the turn does not hold her in the air.
    w.drop_from(300.0);
    let outs = run(&mut w, &world, &mut t, 1.5, 30.0, calm);
    assert!(outs.iter().any(|o| o.touchdown.is_some()));
    assert_eq!(w.ground().1, FLOOR);
}

#[test]
fn after_the_turn_she_goes_on() {
    let world = world(vec![]);
    let (mut w, mut t) = settled((300.0, FLOOR), &world, still_params());
    w.wander_soon(t);
    run(&mut w, &world, &mut t, 0.5, 30.0, FREE);
    let calm = Ctx {
        calm: true,
        free: false,
        ..FREE
    };
    run(&mut w, &world, &mut t, 1.0, 30.0, calm);
    assert_eq!(w.mode(), Mode::Quieta);
    run(&mut w, &world, &mut t, 0.5, 30.0, FREE);
    assert_eq!(w.mode(), Mode::Caminando);
}

#[test]
fn wandering_is_rare_and_she_wakes_up_first() {
    let world = world(vec![]);
    let mut w = Walker::new(Params::default(), SIZE, (300.0, FLOOR), 3);
    let mut t = 0.0;
    // Seven minutes: never less than Wawi 1's 8 between wanders.
    let outs = run(&mut w, &world, &mut t, 7.0 * 60.0, 5.0, FREE);
    assert!(outs.iter().all(|o| o.loco.is_none()));
    assert!(w.trips.is_empty());
    // Asleep when it is time: she asks to be woken, and goes once up.
    let asleep = Ctx {
        asleep: true,
        free: false,
        ..FREE
    };
    let outs = run(&mut w, &world, &mut t, 20.0 * 60.0, 5.0, asleep);
    assert!(outs.iter().any(|o| o.wake));
    assert!(w.trips.is_empty());
    run(&mut w, &world, &mut t, 1.0, 5.0, FREE);
    assert_eq!(w.trips.len(), 1);
}

#[test]
fn she_walks_at_wawi_1s_pace_and_faces_the_way() {
    let world = world(vec![]);
    let (mut w, mut t) = settled((300.0, FLOOR), &world, still_params());
    w.wander_soon(t);
    t += 0.001;
    w.tick(t, &world, FREE);
    let x0 = w.ground().0;
    run(&mut w, &world, &mut t, 1.0, 60.0, FREE);
    let moved = (w.ground().0 - x0).abs();
    assert!((moved - 42.0).abs() < 1.5, "{moved}");
    assert_eq!(w.body().facing_right, w.ground().0 > x0);
}

#[test]
fn climbing_never_repeats_and_waits_its_turn() {
    // One window, up high: from the floor, every trip up is a climb.
    let world = world(vec![win(1, 400.0, 450.0, 700.0, 300.0)]);
    let params = Params {
        wander_every: (60.0, 60.0),
        ..Params::default()
    };
    let (mut w, mut t) = settled((200.0, FLOOR), &world, params);
    for _ in 0..40 {
        w.wander_soon(t);
        run(&mut w, &world, &mut t, 70.0, 10.0, FREE);
    }
    let trips = &w.trips;
    assert!(trips.len() > 10, "{trips:?}");
    for pair in trips.windows(2) {
        assert!(
            !(pair[0] == Trip::Climb && pair[1] == Trip::Climb),
            "{trips:?}"
        );
    }
    let climbs = trips.iter().filter(|t| **t == Trip::Climb).count();
    // 40 wanders over ~47 minutes: one climb per 30 minutes at most.
    assert!(climbs <= 2, "{climbs} climbs: {trips:?}");
}

#[test]
fn the_anti_repetition_rule() {
    let mut w = Walker::new(Params::default(), SIZE, (0.0, 0.0), 1);
    assert!(w.allowed(Trip::Climb, 0.0));
    w.started(Trip::Climb, 0.0);
    // Never twice in a row, and not again for cada_trepada.
    assert!(!w.allowed(Trip::Climb, 0.0));
    w.started(Trip::Walk, 10.0);
    assert!(!w.allowed(Trip::Climb, 29.0 * 60.0));
    assert!(w.allowed(Trip::Climb, 31.0 * 60.0));
    // Walking is the usual: always allowed.
    w.started(Trip::Walk, 11.0);
    assert!(w.allowed(Trip::Walk, 12.0));
    w.started(Trip::Chute, 20.0);
    assert!(!w.allowed(Trip::Chute, 21.0));
    assert!(w.allowed(Trip::Climb, 40.0 * 60.0));
}

#[test]
fn off_a_high_window_she_comes_down_by_parachute() {
    // A wide window high up, with the floor reachable only by going down.
    let world = world(vec![win(1, 0.0, 600.0, 1440.0, 300.0)]);
    let params = Params {
        chute_chance: 1.0,
        wander_every: (1e9, 1e9),
        ..Params::default()
    };
    let (mut w, mut t) = settled((700.0, 600.0), &world, params);
    let floor = *w.ledges.iter().find(|l| l.window.is_none()).unwrap();
    let top = *w.ledges.iter().find(|l| l.window.is_some()).unwrap();
    // A trip down, as `wander` makes it.
    w.body.chute = true;
    w.body.go(plan_path(
        Some(top),
        w.ground(),
        (300.0, FLOOR),
        Some(floor),
        SIZE,
        42.0,
    ));
    let mut fastest: f64 = 0.0;
    let mut saw = false;
    let mut touch = None;
    for out in run(&mut w, &world, &mut t, 40.0, 30.0, FREE) {
        if out.loco == Some(Loco::Chute) {
            saw = true;
            fastest = fastest.max(-w.body().vy);
        }
        touch = touch.or(out.touchdown);
    }
    assert!(saw);
    assert!(fastest <= 75.0 + 1e-9, "{fastest}");
    assert_eq!(touch, Some(Touch::Chute));
}

#[test]
fn a_long_hard_fall_is_a_porrazo_and_a_short_one_is_not() {
    let world = world(vec![]);
    let params = Params {
        flop_from: 500.0,
        ..still_params()
    };
    let (mut w, mut t) = settled((700.0, FLOOR), &world, params);
    w.drop_from(200.0);
    let outs = run(&mut w, &world, &mut t, 1.5, 60.0, FREE);
    assert!(outs.iter().any(|o| o.touchdown == Some(Touch::Soft)));
    w.drop_from(600.0);
    let outs = run(&mut w, &world, &mut t, 1.5, 60.0, FREE);
    assert!(outs.iter().any(|o| o.touchdown == Some(Touch::Flop)));
    assert!(outs.iter().any(|o| o.loco == Some(Loco::Flop)));
}

#[test]
fn rebote_bounces_and_wawi_does_not() {
    let world = world(vec![]);
    let bouncy = Params {
        bounce: 0.4,
        ..still_params()
    };
    let (mut w, mut t) = settled((700.0, FLOOR), &world, bouncy);
    w.drop_from(300.0);
    let mut went_up = false;
    for _ in 0..180 {
        t += 1.0 / 60.0;
        w.tick(t, &world, FREE);
        went_up |= w.body().vy > 0.0;
    }
    assert!(went_up);
    assert_eq!(w.ground().1, FLOOR);
    let (mut w, mut t) = settled((700.0, FLOOR), &world, still_params());
    w.drop_from(300.0);
    for _ in 0..120 {
        t += 1.0 / 60.0;
        w.tick(t, &world, FREE);
        assert!(w.body().vy <= 0.0);
    }
}

#[test]
fn held_she_dangles_and_let_go_she_falls() {
    let world = world(vec![]);
    let (mut w, mut t) = settled((700.0, FLOOR), &world, still_params());
    w.grab();
    w.carry(900.0, 500.0);
    let out = run(&mut w, &world, &mut t, 0.5, 30.0, FREE);
    assert!(out.iter().all(|o| o.loco == Some(Loco::Held)));
    assert_eq!(w.ground(), (900.0, 500.0));
    w.release();
    let outs = run(&mut w, &world, &mut t, 1.5, 30.0, FREE);
    assert!(outs.iter().any(|o| o.loco == Some(Loco::Fall)));
    assert_eq!(w.ground(), (900.0, FLOOR));
}

#[test]
fn reduce_motion_and_full_screen_keep_her_still() {
    let world0 = world(vec![]);
    let (mut w, mut t) = settled((700.0, FLOOR), &world0, still_params());
    w.drop_from(400.0);
    let still = Ctx {
        still: true,
        ..FREE
    };
    let outs = run(&mut w, &world0, &mut t, 0.1, 30.0, still);
    assert!(outs.iter().all(|o| o.loco != Some(Loco::Fall)));
    assert_eq!(w.ground().1, FLOOR);
    // A full-screen app in front: no wandering.
    let full = Snapshot {
        version: 9,
        screens: vec![SCREEN],
        windows: vec![Win {
            id: 5,
            rect: SCREEN.frame,
        }],
    };
    w.wander_soon(t);
    run(&mut w, &full, &mut t, 5.0, 10.0, FREE);
    assert!(w.trips.is_empty());
}

#[test]
fn without_screens_nothing_moves() {
    let none = Snapshot::default();
    let mut w = Walker::new(Params::default(), SIZE, (500.0, 500.0), 1);
    let out = w.tick(0.0, &none, FREE);
    w.tick(10.0, &none, FREE);
    assert_eq!(out.ground, (500.0, 500.0));
    assert_eq!(w.ground(), (500.0, 500.0));
}

#[test]
fn a_climb_goes_up_the_side_and_onto_the_top() {
    let w1 = win(1, 600.0, 400.0, 500.0, 300.0);
    let ls = ledges(&[SCREEN], std::slice::from_ref(&w1), SIZE);
    let top = *ls.iter().find(|l| l.window == Some(1)).unwrap();
    let floor = *ls.iter().find(|l| l.window.is_none()).unwrap();
    let legs = plan_path(
        Some(floor),
        (200.0, FLOOR),
        (800.0, 400.0),
        Some(top),
        SIZE,
        42.0,
    );
    assert!(matches!(legs[0], Leg::Walk { to, .. } if to < 600.0));
    assert!(matches!(legs[1], Leg::Climb { to } if to == 400.0));
    assert!(matches!(legs[2], Leg::Shift { .. }));
    assert!(matches!(legs[3], Leg::Walk { to, .. } if to == 800.0));
    // And down: off the end of the window, then gravity.
    let legs = plan_path(
        Some(top),
        (800.0, 400.0),
        (200.0, FLOOR),
        Some(floor),
        SIZE,
        42.0,
    );
    assert!(matches!(legs.last(), Some(Leg::WalkOff { .. })));
}

#[test]
fn a_trip_down_goes_on_after_the_landing() {
    let world = world(vec![win(1, 600.0, 400.0, 500.0, 300.0)]);
    let (mut w, mut t) = settled((850.0, 400.0), &world, still_params());
    let floor = *w.ledges.iter().find(|l| l.window.is_none()).unwrap();
    let legs = plan_path(
        support(&w.ledges, 850.0, 400.0),
        w.ground(),
        (200.0, FLOOR),
        Some(floor),
        SIZE,
        42.0,
    );
    w.body.go(legs);
    w.goal = Some(Goal {
        target: (200.0, FLOOR),
        ledge: Some(floor),
        replans: 0,
    });
    run(&mut w, &world, &mut t, 40.0, 30.0, FREE);
    assert!((w.ground().0 - 200.0).abs() < 1.0, "{:?}", w.ground());
    assert_eq!(w.ground().1, FLOOR);
}
