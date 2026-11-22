# hark

A lean terminal podcast player. Open podcasts (any public RSS feed), played in-process: no mpv,
no accounts, nothing running while idle. Built on the same principles as
[trill](https://github.com/junaydirfan/trill).

**Status:** foundations. The feed reader and the audio engine work and are measured; the TUI
comes next. See [PLAN.md](PLAN.md).

## Try the pieces

```sh
cargo build --release
./target/release/hark feed https://feeds.npr.org/510289/podcast.xml   # fetch + parse, with timings
./target/release/hark play episode.mp3 1.5 600                         # 1.5× from 10:00 (seek, pause, resume test)
```

## Numbers so far

| | |
|---|---|
| Parse a 20.7 MB feed (3,044 episodes) | 19 ms |
| Load a local episode to playing | 10 ms |
| Memory while playing | 4.7 MB |
| CPU playing at 1.5× | ~1.3% of one core |
| CPU paused | ~0 |
| Binary | 1.9 MB |

Speed changes keep the pitch: [Sonic](https://github.com/waywardgeek/sonic) (via `libsonic`), the algorithm made for speeding up speech.
