# hark

A lean terminal podcast player. Open podcasts (any public RSS feed), played in-process: no mpv,
no accounts, nothing running while paused. Built on the same principles as
[trill](https://github.com/junaydirfan/trill).

**Status:** Phases 1 and 2 of [PLAN.md](PLAN.md) are done. Next: chapters, synced transcripts,
downloads, sleep timer.

## Get started

```sh
cargo build --release
./target/release/hark
```

Press `/` and search for a podcast, or open **Discover** in the sidebar. You can also paste a
feed URL, or the path to an OPML file exported from another podcast app (AntennaPod, Pocket Casts,
Overcast, Apple Podcasts…) to bring all your shows over.

## What's in it

- **Search** (`/`) every podcast in the Apple Podcasts directory.
- **Discover:** "For you", then the top charts for your country. `o` / `O` flips through genres
  (Business, Comedy, True Crime, Technology…).
- **Preview before you follow:** open any show, browse and play its episodes, then `f` to follow it.
- **Inbox:** every unplayed episode from your shows in the last 30 days, newest first.
- **In progress:** what you started and didn't finish, most recent first, with the time left.
- **Queue:** `a` adds an episode, `A` plays it next. When an episode ends, the next one starts. It's saved between runs.
- **Resume anywhere:** your place is saved on pause, seek, switch and quit, and every 30 s.
- **Speed without chipmunks:** 0.5–3×, pitch kept ([Sonic](https://github.com/waywardgeek/sonic)).
- **Show notes:** `L` shows the highlighted episode's notes as clean text, with its links listed below.
- **Cover art** of the playing episode under the library: real images in kitty, Ghostty, WezTerm,
  foot, iTerm2 and Konsole, colour half-blocks everywhere else (`i` hides it).
- **Desktop native (Linux):** media keys, the KDE/GNOME media widget, the lock screen, `playerctl`
  and KDE Connect, all through MPRIS. Speed shows up as the playback rate, so the widget can change it too.
- **trill's look:** the same layout, the help popup on `?`, colour themes on `t`, and the mouse (click to open,
  double-click to play, click the bar to seek, scroll the pane under the pointer).

## Keys

Press `?` in hark for all of them.

| key | action | | key | action |
|---|---|---|---|---|
| `↑↓` `jk` `PgUp/PgDn` `g/G` | move | | `Space` | play / pause |
| `Tab` `h` `l` | switch pane | | `←` `,` / `→` `.` | back 15 s / forward 30 s |
| `Enter` | open / play | | `p` | back to the start |
| `Esc` | back | | `[` / `]` | speed −/+ 0.1× |
| `w` | jump to the playing episode | | `+` / `-` | volume ±5% |
| `a` / `A` | queue / play next | | `n` | next in the queue |
| `m` | mark played / unplayed | | `L` | show notes |
| `/` | search · paste a feed URL / OPML | | `R` | refresh every show now |
| `f` | follow the highlighted show | | `o` / `O` | Discover: next / previous genre |
| `D` `D` | unsubscribe | | `t` / `i` | next colour theme / cover on-off |
| `q` `Ctrl-C` | quit (your place is saved) | | | |

In lists: `▶` playing, `◐` started (with the time left), `✓` played, `≡` queued.

## Privacy

hark has no account and sends no analytics. It talks to the servers of the shows you follow (for
their feeds and audio), and to Apple's public podcast directory only for Discover and search:

- **Search** sends what you type.
- **Charts** send a genre and your country (taken from your locale, e.g. `en_CA` → Canada).
- **"For you"** is built from the categories your own feeds declare. hark fetches the charts for
  those genres and leaves out what you follow, so **your list of shows never leaves your machine**.

## Where things live

- `~/.local/share/hark/subscriptions.opml`: your shows, as standard OPML, so you can take them anywhere.
- `~/.local/share/hark/state.tsv`: where you are in each episode you've started, and what you've finished.
- `~/.local/share/hark/queue.tsv`: the queue.
- `~/.local/share/hark/prefs`: speed, volume and theme.
- `~/.cache/hark/`: feed caches, show notes, and the episode streaming right now. Safe to delete.

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
| Memory: idle / streaming | 11 MB / 16–18 MB |
| CPU playing (incl. the once-a-second redraw) | ~2% of one core |
| CPU paused | 0 |
| Memory with the cover drawn | ~15 MB |
| Binary | 4.7 MB |

How it stays light:
- One async thread for the screen and network; input and audio each sleep on their own thread
  until there's something to do. The screen redraws only when something changes.
- Audio is decoded in-process (symphonia). Speed changes keep the pitch with
  [Sonic](https://github.com/waywardgeek/sonic), the algorithm made for speeding up speech.
- Pausing closes the audio output, so nothing wakes up while paused.
- Only the rows that fit on screen are drawn, so a 3,000-episode show scrolls as fast as a short one.
- Show notes stay on disk and are read one episode at a time, when you open them.
- Big feeds don't leave their memory behind: freed memory goes back to the OS, and big buffers
  never land in the heap.

## Developer commands

```sh
hark feed <url|file>                             # fetch + parse a feed, with timings
hark play <url|file> [speed] [start_s] [volume]  # engine test: seek, pause, resume, with memory and CPU
```
