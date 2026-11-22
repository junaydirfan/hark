# hark plan

A terminal podcast player for open podcasts (public RSS feeds), built on trill's principles:
do one thing, cost nothing while idle, and be fast enough that nobody waits.

Out of scope: Spotify/Audible exclusives and video podcasts (no public feed).

## Principles (from trill)

| trill | hark |
|---|---|
| One async thread for UI and network; input on its own blocking thread | Same |
| Redraws only on change (1/s while playing); never polls | Same. Feeds refresh at launch when stale, on `R`, and about hourly while open |
| First frame drawn from the disk cache (~30 ms) | Same: shows and episodes come from cache, then refresh quietly |
| Batched, parallel network requests | 8–16 feeds at a time, conditional GET (ETag / Last-Modified → 304) |
| Builds only the rows on screen | Same; 3,000-episode shows stay fast |
| Cover art decoded once, off the UI thread; next one prefetched | Same, plus resized covers cached on disk |
| `malloc_trim`, 2 malloc arenas, fat LTO, `panic = "abort"`, strip | Same |
| MPRIS / macOS Now Playing | Port trill's `mpris.rs` |

Also worth reusing from trill: mouse handling, the config parser, the album-art picker, the
update checker and the CI workflow (already copied).

## Audio engine: in-process, not mpv (measured 2026-11-22)

Same NPR episode (128 kbps MP3), silent, i9-14900HX:

| | mpv (stripped down, small caches) | hark engine (symphonia + Sonic + cpal) |
|---|---|---|
| Memory (PSS) | ~61 MB | **4.7 MB for all of hark** |
| CPU playing at 1.5× | 2.6% | **1.3%** |
| CPU paused | 0 | ~0 (one clock tick in 3 s) |
| Load to playing | ~0.65 s (streaming) | 10 ms (local file) |
| Extra install on macOS/Windows | mpv | none |

- symphonia decodes (MP3 now; add `isomp4` + `aac` for M4A feeds).
- libsonic (a port of Sonic, the speech-speed algorithm) changes the speed without changing the pitch.
- cpal plays; on Linux through PulseAudio/PipeWire directly.
- The audio thread sleeps until the output drops below 0.25 s queued, then decodes back up to
  0.5 s. Paused or idle, it waits on a command with no timer.

Known gaps:
- **Streaming:** the engine plays files. It still needs a source that downloads progressively
  to `~/.cache/hark/` while playing, and uses range requests to jump ahead on a seek.
- **Resampling for macOS/Windows:** the output uses the file's own sample rate and f32 samples.
  PulseAudio converts on Linux; CoreAudio/WASAPI may refuse, so those need a resampler (rubato).
- **VBR seeking:** MP3 seeks are coarse (they use the bitrate). That's fine for constant-bitrate
  files, but it can land a few seconds off on VBR without a Xing table of contents.

## Feeds (measured)

One streaming pass with quick-xml, keeping only the fields hark uses:

| Feed | Size | Episodes | Parse |
|---|---|---|---|
| Planet Money | 2.2 MB (300 KB gzipped) | 355 | 3 ms |
| Lex Fridman | 2.1 MB | 505 | 4 ms |
| The Daily | **20.7 MB** | **3,044** | **19 ms** |

It reads title, guid, date, duration, the enclosure (url, type, size), episode image, notes,
`podcast:chapters`, and `podcast:transcript` (timed formats first), plus the show title,
author, image and `itunes:new-feed-url`. Items with no audio are skipped.

## Data on disk

- `~/.config/hark/config`: settings (`key = value`, as in trill).
- `~/.local/share/hark/subscriptions.opml`: subscriptions as standard OPML (portable).
- `~/.local/share/hark/state.tsv`: guid → position, played, updated. Only for episodes you've
  touched.
- `~/.cache/hark/feeds/<id>.tsv`: parsed episodes plus ETag/Last-Modified. Notes go in a
  separate file, loaded on demand.
- `~/.cache/hark/art/`: resized covers. `~/.local/share/hark/downloads/`: offline episodes.
- Every write goes to a temporary file and is then renamed into place. Position is saved on
  pause, seek and quit, and every 30 s while playing.

## Screens

- **Sidebar:** Inbox (new from every show), In progress, Queue, Downloads, then shows (newest
  episode first).
- **Episode list:** ● new / ◐ started (time left) / ✓ played, title, date, length.
- **Now playing:** title · show · chapter, a clickable progress bar, speed, time left, sleep timer.
- **Keys:** the same as trill where the meaning is the same. Podcast extras: 15 s back / 30 s
  forward, `[` `]` speed, a transcript view that follows along like trill's lyrics.

## Phases

1. **It plays.** Streaming source; add a feed by URL or import OPML; refresh; episode list (TUI);
   play / pause / seek / skip / speed / volume; resume positions; mark played; cache-first
   start. *Done when:* you can add your shows, quit mid-episode and resume exactly there.
2. **Daily driver.** Inbox, In progress and Queue (auto-continue); directory search (Apple's
   iTunes Search API needs no key and is contacted only when you search); show notes; art;
   MPRIS + media keys; mouse; background refresh; OPML export.
3. **Best at it.** Chapters (Podcasting 2.0 JSON + ID3 CHAP); synced transcripts; downloads
   (resumable, keep newest N); per-show speed and intro/outro skip; sleep timer; loudness
   levelling.
4. **Sync.** gPodder-compatible sync (gpodder.net / Nextcloud) with phone apps like
   AntennaPod; Windows media controls.

## Targets

Launch to list < 30 ms (from cache) · own RAM < 15 MB with 50 shows / 10k episodes · idle CPU 0 ·
playing < 1.5% at 1.5× · refresh 50 unchanged feeds < 1 s · binary < 8 MB (1.9 MB today).

## Pitfalls to handle

Broken XML and odd dates or durations (fill the length from the decoder once played); 301s and
`itunes:new-feed-url` (save the new address); dynamically inserted ads (the same episode is a
different file each time, so a saved position can drift; downloads avoid it); huge feeds;
Windows audio formats. Privacy: hark only talks to feed and audio hosts, plus the directory when
you search.
