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
update checker and the CI workflow (both copied).

## Audio engine: in-process, not mpv (measured 2026-11-22)

Same NPR episode (128 kbps MP3), silent, i9-14900HX:

| | mpv (stripped down, small caches) | hark engine (symphonia + Sonic + cpal) |
|---|---|---|
| Memory (PSS) | ~61 MB | **4.7 MB for all of hark** |
| CPU playing at 1.5× | 2.6% | **1.3%** |
| CPU paused | 0 | ~0 (one clock tick in 3 s) |
| Load to playing | ~0.65 s (streaming) | 10 ms (local file) |
| Extra install on macOS/Windows | mpv | none |

- symphonia decodes MP3 and M4A/AAC (`isomp4` + `aac`, +0.35 MB).
- libsonic (a port of Sonic, the speech-speed algorithm) changes the speed without changing the pitch.
- cpal plays; on Linux through PulseAudio/PipeWire directly.
- The audio thread sleeps until the output drops below 0.25 s queued, then decodes back up to
  0.5 s. Paused or idle, it waits on a command with no timer.

Known gaps:
- **Streaming (done):** a download task writes the episode into `~/.cache/hark/tmp/` while it
  plays. Seeking past what has arrived restarts the download there with a range request. The
  engine decodes only with 64 KB ready (otherwise it reports `Buffering`), so a slow network
  never blocks pause or seek. It retries 3 times. Measured: ~0.6 s from url to playing, 8.1 MB.
  Gap behind a forward seek is only fetched when playback seeks back into it.
- **Sample rates:** the output uses the file's own rate in f32. PulseAudio converts on Linux, cpal
  opens WASAPI with AUTOCONVERTPCM on Windows, and CoreAudio switches the device's rate on macOS,
  so no resampler is needed (checked in cpal 0.18's source; not yet heard on real hardware).
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

1. **It plays. (done, 2026-11-22)** Streaming; add a feed by URL or import OPML; refresh (conditional
   GET, plus same-ETag skip for servers that ignore it); episode list (TUI); play / pause / seek /
   skip / speed / volume; resume positions; mark played; unsubscribe; cache-first start.
   Measured with 3 shows / 3,904 episodes: 11 MB idle, 16 MB streaming, ~2% CPU playing, 0 paused.
   Lessons: keep show notes out of memory until shown; copy parsed episodes onto the main thread
   (the parse thread's arena can then be trimmed); pin `M_MMAP_THRESHOLD`; close the audio output
   while paused (cpal's PulseAudio thread keeps waking otherwise).
2. **Daily driver. (done, 2026-11-22, except OPML export and background refresh)** trill's look
   (logo, Library sidebar, help, themes, mouse); Inbox (30 days), In progress, Queue with auto-continue
   and play-next; show notes (`L`, on disk per feed, read one episode at a time); Discover (Apple's
   charts by country and genre, `o`/`O`) and search; "For you" from your feeds' own categories (your
   show list never leaves the machine); preview before following (`f`); cover art; MPRIS on Linux
   with speed as the playback rate. OPML export (`hark --export`), the hourly refresh
   while open and macOS Now Playing followed on 2026-11-23. Still to do: Windows media controls.
3. **Best at it. (mostly done, 2026-11-24)** Chapters (Podcasting 2.0 JSON, else ID3 CHAP through
   symphonia's `id3v2` reader; `c`/`C`); downloads (`d`, resumable with `If-Range` on a strong ETag
   only, since dynamically inserted ads make every request a different file; `K` keeps the newest
   1/3/5; played ones delete themselves); per-show speed (`e`) and intro/outro skip (`I`/`E`);
   sleep timer (`z`, 10 s fade). Still to do: synced transcripts (the link is already kept with
   the notes), loudness levelling.
4. **Sync.** gPodder-compatible sync (gpodder.net / Nextcloud) with phone apps like
   AntennaPod; Windows media controls.

## Progress (2026-11-23)

Saving your place is the feature podcast listeners care about most, so it's hardened. The
position is saved every 10 s while playing, plus on pause, seek, switch, quit and every
"stop" signal (terminal closed, logout, kill). Tested: closing the terminal saved the exact
second; `kill -9` lost 6 s. Smart resume rewinds 3/10/20 s after a minute/hour/day away. The
outro (last 30 s) counts as finished. The length the decoder measures replaces the feed's.
Next: sync with phone apps (gPodder, Phase 4), so your place follows you.

## Recommendations (2026-11-23)

For you weights each followed show by 1 + hours listened (fading by half every 90 days past
the first 90), spreads that over the show's subgenres (its broad genre counts half), asks for
the top 4 charts in parallel (12 h disk cache; a failed chart is skipped, a stale copy beats
none), scores `weight / (1 + rank/10)` summed over charts, × freshness (1 / 0.6 / 0.2 for a
last episode within 30 / 90 / older days), and picks greedily with each genre damped 15% per
pick. `x` hides a show for good; `M` searches the maker. The Inbox lifts a show you listen to
by up to 3 days. All signals are local; Apple sees only chart genres.

## Memory (2026-11-24)

15 shows / 16,900 episodes, measured as PSS: 20.0 → 16.6 MB from cache, 33.0 → 24.5 MB after
refreshing every feed, refresh peak 66 → 58 MB. An episode in memory is 64 bytes (was 240):
guid, title, url as `Box<str>`, date, length. Notes, chapters, transcript and the episode's own
cover (only when it isn't the show's) live in the per-feed notes file, read one episode at a
time; the file type and size are dropped. What's left after a full refresh: ~2 MB of glibc
fragmentation and ~2 MB in use by OpenSSL's root store once anything went online.

Startup: a terminal that doesn't answer the image query cost 0.5 s every launch (tmux among
them). It's remembered for a week (`~/.cache/hark/terminal`, keyed by TERM/TERM_PROGRAM/version)
and half-blocks are used straight away: first frame 537 → 26 ms.

## Targets

Launch to list < 30 ms (from cache) · own RAM < 15 MB with 50 shows / 10k episodes · idle CPU 0 ·
playing < 1.5% at 1.5× · refresh 50 unchanged feeds < 1 s · binary < 8 MB (1.9 MB today).

## Pitfalls to handle

Broken XML and odd dates or durations (fill the length from the decoder once played); 301s and
`itunes:new-feed-url` (save the new address); dynamically inserted ads (the same episode is a
different file each time, so a saved position can drift; downloads avoid it); huge feeds;
Windows audio formats. Privacy: hark only talks to feed and audio hosts, plus the directory when
you search.
