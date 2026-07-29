# Overlay performance: measured, not assumed

Measured on an M4 Pro, macOS 26.5, 120 Hz ProMotion display, release build.

The short version: **the overlay is not a performance problem, and the
optimisations that looked obvious from reading the brief were already done.**
This document exists so the next person does not spend an afternoon
rediscovering that.

## Frame cost

`OVERLAY_FRAMESTATS=1` is built into the overlay and reports real numbers.
Three consecutive four-second windows while driving the real overlay through
`overlay-proto`:

```
465 ticks | interval p50 8.33ms p95 8.37ms max 18.42ms | draw p50 0.52ms p95 0.84ms
478 ticks | interval p50 8.33ms p95 8.35ms max 27.86ms | draw p50 0.69ms p95 1.00ms
481 ticks | interval p50 8.33ms p95 8.35ms max  8.68ms | draw p50 0.73ms p95 1.10ms
```

An 8.33ms interval is **120 Hz**, so the clock is vsync-aligned to ProMotion
rather than running a fixed 30 or 60 Hz timer. Drawing consumes **0.52-0.73ms
of an 8.33ms budget**, roughly 8%. Even the p95 worst case leaves seven
eighths of the frame idle.

Two `max` outliers (18.4ms, 27.9ms) are dropped frames under contention from
other work on the machine, not a systematic cost.

## Idle cost

The question that matters more for a background utility: does it burn CPU
when nothing is happening?

```
$ ps -p 53270 -o pcpu,rss,etime
  0.0  12144   49:23
```

**0.0% CPU and 12 MB resident after 49 minutes idle.** The backlog target was
"idle < 1% CPU"; actual is zero to the resolution `ps` reports.

That is by construction rather than luck. `macos.rs` invalidates the clock on
hide, so a hidden overlay schedules nothing at all:

```rust
fn stop_clock(&mut self) {
    Some(Clock::DisplayLink(link)) => msg_send![&*link, invalidate],
    Some(Clock::Timer(timer))      => timer.invalidate(),
}
```

## Optimisations that were already in place

Everything on the obvious list had been done before this investigation:

| Suspected waste | Actual state |
|---|---|
| Fixed-rate timer ignoring vsync | `CVDisplayLink`, with an `NSTimer` fallback |
| Clock runs while hidden | Invalidated on hide |
| Per-frame allocation | Paths, colours, and fonts cached |
| Full-panel invalidation | Dirty-rect drawing |

## What this means for further work

There is no meaningful performance win available in the frame loop. At 0.73ms
per frame and 0% idle, effort spent shaving microseconds there is effort not
spent on something a user would notice.

Moving the glow into a `CALayer` so Core Animation drives it on the window
server remains theoretically attractive, and it is the one change that could
still matter, but the justification is no longer speed. It would be about
smoothness under system load, since a CA animation keeps running when our
process is briefly starved. That is a real but narrow benefit, and it should
be weighed against the complexity of splitting the render between our
`drawRect` and a layer tree.

**Visual depth remains genuinely open.** It is a design problem, not a
performance one, and the frame budget has ample room for the extra shadow and
gradient passes it needs: even tripling current draw cost would stay under 25%
of a 120 Hz frame.

## Reproducing

```bash
OVERLAY_FRAMESTATS=1 cargo run --release -p overlay --bin overlay-proto
ps -p "$(pgrep -f 'outloud' | head -1)" -o pcpu,rss,etime
```
