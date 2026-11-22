# hark

A lean terminal podcast player. Open podcasts (any public RSS feed), played in-process: no mpv,
no accounts, nothing running while idle. Built on the same principles as
[trill](https://github.com/junaydirfan/trill).

**Status:** foundations. The feed reader and the audio engine (local files and streaming) work
and are measured; the TUI comes next. See [PLAN.md](PLAN.md).

## Try the pieces

```sh
cargo build --release
./target/release/hark feed https://feeds.npr.org/510289/podcast.xml   # fetch + parse, with timings
./target/release/hark play <episode url or file> 1.5 600               # 1.5× from 10:00 (seek, pause, resume test)
```

## Numbers so far

| | |
|---|---|
| Parse a 20.7 MB feed (3,044 episodes) | 19 ms |
| Load a local episode to playing | 10 ms |
| Stream an episode to playing (through its tracking redirects) | ~0.6 s |
| Start a stream at 22:00 (range request) | ~0.6 s, ~20 ms buffering |
| Memory while playing | 4.7 MB local, 8.1 MB streaming |
| CPU playing at 1.5× | ~1.3% of one core |
| CPU paused | ~0 |
| Binary | 2.0 MB |

Speed changes keep the pitch: [Sonic](https://github.com/waywardgeek/sonic) (via `libsonic`), the algorithm made for speeding up speech.
