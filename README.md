# dino-facts

A keyless dinosaur trivia API in the spirit of [wttr.in](https://github.com/chubin/wttr.in).
One small Rust binary serves bite-sized facts harvested from Wikipedia to terminals, scripts and browsers.

```text
$ curl -s localhost:8080/ankylosaurus

  🦕 DINO TRIVIA: ANKYLOSAURUS (Description)
  ╭──────────────────────────────────────────────────────╮
  │ The club of the largest specimen may have been 57 cm │
  │ (22+1⁄2 in) wide.                                    │
  ╰───────────────────┬──────────────────────────────────╯
```

## Usage

| Request | Result |
|---|---|
| `curl localhost:8080/` | Random fact as an ANSI speech-bubble card |
| `curl localhost:8080/stegosaurus` | Random fact about one dinosaur |
| `curl "localhost:8080/?q"` | One plain line, for `.bashrc` or a MOTD |
| `curl "localhost:8080/?format=json"` | `{dino, category, fact, wiki_url}` |
| `curl "localhost:8080/?format=text"` | Force the terminal card |
| browser | Dark-mode HTML card |
| `/stats` | Last-24h requests, Wikipedia calls and reservoir size |

`curl`, `wget`, `httpie` and `fetch` user agents get the terminal card automatically.
The intended public address is `dinofacts.fyi` (not deployed yet).

## Run

```bash
docker compose up --build -d
```

The compose file also starts an [Ollama](https://ollama.com) sidecar and pulls a ~2 GB model on first start.
To run without it, delete the `ollama` service and the `OLLAMA_*` lines; facts then come from the heuristic ranker only.

Without Docker: `cargo run --release` (listens on `0.0.0.0:8080`).

## Configuration

| Variable | Default | Purpose |
|---|---|---|
| `BIND_ADDR` | `0.0.0.0:8080` | Listen address |
| `WIKI_USER_AGENT` | `dino-trivia/1.0 (https://dinofacts.fyi; hello@radomski.dev) reqwest/0.12` | Sent to Wikipedia; Wikimedia expects a real contact |
| `OLLAMA_URL` | unset | Enables LLM fact extraction, e.g. `http://ollama:11434` |
| `OLLAMA_MODEL` | `qwen2.5:3b-instruct` | Model the worker asks for |
| `CLIENT_IP_HEADER` | unset | Header carrying the real client IP behind a proxy (e.g. `CF-Connecting-IP`); unset keys the rate limit on the connecting address |
| `OLLAMA_NUM_THREAD` | `2` | Inference threads; keep equal to the sidecar's CPU limit (sized for a 2 vCPU / 8 GB host) |

## How it works

- **Reservoir.** `GET /` reads a random fact from an in-memory pool (capped at 5,000 facts), so requests never wait on the network. Re-harvesting a dinosaur replaces its facts, and when the pool is full random old facts are evicted so it keeps rotating.
- **Harvester.** A background task reads the ~1,400 genera in Wikipedia's "Dinosaur genera" category, harvests a built-in set of well-known ones first and the rest in random order (one new article per 45 s, so the full list takes about 17 hours), and fills the pool and a per-dinosaur cache. Cache entries expire after 7 days and are refetched on the next pass. `GET /{dino}` harvests on demand; concurrent misses for one name share a single fetch.
- **Wikipedia etiquette.** Custom User-Agent, at most 2 concurrent requests and a 350 ms pause between calls.
- **Ranking** (`src/extract.rs`). Sentences are scored on trivia vocabulary, measurements, superlatives and whether they name the dinosaur. Scholarly hedging, specimen codes, citations and context-dependent openers are rejected, and the best 8 per dinosaur are kept.
- **LLM upgrade** (`src/llm.rs`, optional). After the heuristic facts are cached, one worker asks the model for 3-5 facts per article and swaps them in. A fact is kept only if every number and about 85% of its longer words appear in the source article, so invented trivia is dropped. If the sidecar is down or returns too little, the heuristic facts stay.
- **Telemetry** (`src/stats.rs`). A fixed ring of 1,440 one-minute atomic slots covers 24 hours with no allocation and no disk writes.
- **Limits.** Per-IP rate limit with a burst of 40, then 3 requests/s (one every 333 ms); over-limit requests get an immediate `429` with `Retry-After`. No database, no external state.

## Running behind a proxy

By default the rate limiter keys on the connecting address, so behind Traefik or Cloudflare every visitor would share one bucket. Set `CLIENT_IP_HEADER` to the header your proxy overwrites with the real client IP (`CF-Connecting-IP` behind Cloudflare). Only do this if the origin is reachable solely through that proxy; otherwise a client can forge the header to dodge the limit. A missing or unparsable header falls back to the connecting address.

## Development

```bash
cargo test
```

Tests run in memory with no network access: the extractor, grounding check, stats ring, renderer and routes are all covered.

```text
src/main.rs     wiring, handlers, Wikipedia harvesting
src/extract.rs  sentence splitting, scoring and ranking
src/genera.rs   genus discovery from Wikipedia's category
src/llm.rs      Ollama worker and grounding check
src/render.rs   ANSI, HTML, JSON and stats rendering
src/reservoir.rs bounded, rotating pool behind GET /
src/stats.rs    24h rolling telemetry
```

## Attribution

Facts come from Wikipedia and are available under [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/); each fact links back to its source article.
