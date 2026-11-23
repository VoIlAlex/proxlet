# Benchmarks

A quick check that proxlet adds little overhead and stays error-free under load.
These numbers are not a ceiling: the load generator was the bottleneck (see
[Caveats](#caveats)).

## Setup

- **Machine:** Apple M3 Max, 14 cores, macOS. Client and servers on the same host
  over loopback.
- **Build:** `cargo build --release` (thin LTO, one codegen unit).
- **Client:** `ab -k` (ApacheBench, keep-alive).
- **Servers:** two `proxlet run` instances.
  - **A** serves a directory: `proxlet add / ./site`. This is the *files* rule,
    and the direct-to-upstream baseline.
  - **B** proxies everything to A: `proxlet add / 19001`. This is the *proxy*
    rule, so B's numbers are A's plus one proxy hop.
- **Files:** a 15-byte text file and a 100 KiB binary file.

## Results

| Test                         | Concurrency | Requests/s | Mean    | p50   | p99   | Failed |
| ---------------------------- | ----------: | ---------: | ------: | ----: | ----: | -----: |
| files rule, 15 B             | 64          | 27,782     | 2.3 ms  | 2 ms  | 5 ms  | 0      |
| proxy → files rule, 15 B     | 64          | 25,759     | 2.5 ms  | 2 ms  | 5 ms  | 0      |
| files rule, 15 B             | 256         | 27,732     | 9.2 ms  | 8 ms  | 26 ms | 0      |
| proxy → files rule, 15 B     | 256         | 22,875     | 11.2 ms | 10 ms | 29 ms | 0      |
| files rule, 100 KiB          | 64          | 19,243     | 3.3 ms  | 3 ms  | 7 ms  | 0      |
| proxy → files rule, 100 KiB  | 64          | 18,075     | 3.5 ms  | 3 ms  | 7 ms  | 0      |

There were 1.1 million requests in total, with no failures and no non-2xx responses.

**Proxy overhead.** The extra hop costs about 0.2 ms at the mean for small
responses at 64 connections; p99 is unchanged. At 256 connections the proxy runs
at 82% of the direct rate, and p99 rises by 3 ms. With 100 KiB bodies the proxy
keeps 94% of the direct rate, because bodies are streamed rather than buffered.

**Memory.** After the 1.1M requests, resident memory was about 30 MiB for the
files server and 51 MiB for the proxy, which holds the pooled upstream
connections. Idle, a fresh instance uses about 5 MiB.

## Caveats

- ApacheBench is single-threaded and saturates at around 30k requests/s on this
  machine. Direct serving plateaus at the same rate at 64 and 256 connections,
  which is the signature of a client limit. Expect higher absolute numbers with
  a multi-threaded generator such as `oha` or `wrk`.
- Everything ran on one machine, so client, proxy and upstream competed for the
  same cores.
- A proxlet instance served as the upstream. With a slower upstream, its own
  latency dominates and the proxy hop matters even less.

## Reproduce

```sh
cargo build --release
P=target/release/proxlet

mkdir -p /tmp/bench-a/site
printf 'hello, proxlet\n' > /tmp/bench-a/site/hello.txt
head -c 102400 /dev/urandom > /tmp/bench-a/site/100k.bin

export PROXLET_HOME=/tmp/bench-a
$P config listen.address 127.0.0.1
$P config listen.http_port 19001
$P add / /tmp/bench-a/site
$P run &

export PROXLET_HOME=/tmp/bench-b
$P config listen.address 127.0.0.1
$P config listen.http_port 19002
$P add / 19001
$P run &

ab -k -n 200000 -c 64 http://127.0.0.1:19001/hello.txt   # files rule
ab -k -n 200000 -c 64 http://127.0.0.1:19002/hello.txt   # proxy rule
```
