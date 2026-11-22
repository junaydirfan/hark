# hark

A lean terminal podcast player. Open podcasts (any public RSS feed), played in-process: no mpv,
no accounts, nothing running while paused. Built on the same principles as
[trill](https://github.com/junaydirfan/trill).

**Status:** Phase 1 of [PLAN.md](PLAN.md). Subscribe, browse, stream, and pick up exactly where
you left off. Next: an Inbox, a queue, show notes, art and media keys.

## Get started

```sh
cargo build --release
./target/release/hark
```

Press `a`, then paste a podcast's feed URL, or the path to an OPML file exported from another
podcast app (AntennaPod, Pocket Casts, Overcast, Apple Podcasts…).

## Keys

| key | action | | key | action |
|---|---|---|---|---|
| `↑↓` `jk` `PgUp/PgDn` `g/G` | move | | `Space` | play / pause |
| `Tab` `h` `l` | switch pane | | `←` `,` / `→` `.` | back 15 s / forward 30 s |
| `Enter` | open show / play episode | | `[` / `]` | speed −/+ 0.1× (pitch stays) |
| `Esc` | back to shows | | `+` / `-` | volume ±5% |
| `a` | add a feed URL or import OPML | | `m` | mark played / unplayed |
| `R` | refresh every show now | | `D` `D` | unsubscribe |
| `q` `Ctrl-C` | quit (your place is saved) | | | |

In the list: `▶` playing, `◐` started (with the time left), `✓` played.

## Where things live

- `~/.local/share/hark/subscriptions.opml`: your shows, as standard OPML, so you can take them anywhere.
- `~/.local/share/hark/state.tsv`: where you are in each episode you've started, and what you've finished.
- `~/.local/share/hark/prefs`: speed and volume.
- `~/.cache/hark/`: feed caches and the episode streaming right now. Safe to delete.

Feeds refresh when hark starts if they're over an hour old, and on `R`. hark only asks for a feed
"if changed", and most servers then answer with nothing at all.

## Numbers

Measured on an i9-14900HX with three shows (Planet Money, Lex Fridman, The Daily: 3,904 episodes).

| | |
|---|---|
| Start, with your shows on screen (from cache) | first frame, before any network |
| Parse a 20.7 MB feed (3,044 episodes) | 19 ms |
| Stream an episode to playing (through its tracking redirects) | ~0.6 s |
| Start a stream at 22:00 (range request) | ~0.6 s, ~20 ms buffering |
| Memory: idle / streaming | 11 MB / 16 MB |
| CPU playing (incl. the once-a-second redraw) | ~2% of one core |
| CPU paused | 0 |
| Binary | 2.4 MB |

How it stays light:
- One async thread for the screen and network; input and audio each sleep on their own thread
  until there's something to do. The screen redraws only when something changes.
- Audio is decoded in-process (symphonia). Speed changes keep the pitch with
  [Sonic](https://github.com/waywardgeek/sonic), the algorithm made for speeding up speech.
- Pausing closes the audio output, so nothing wakes up while paused.
- Only the rows that fit on screen are drawn, so a 3,000-episode show scrolls as fast as a short one.
- Big feeds don't leave their memory behind: freed memory goes back to the OS, and big buffers
  never land in the heap.

## Developer commands

```sh
hark feed <url|file>                             # fetch + parse a feed, with timings
hark play <url|file> [speed] [start_s] [volume]  # engine test: seek, pause, resume, with memory and CPU
```
