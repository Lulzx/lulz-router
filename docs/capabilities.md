# Capability matching

The gateway doesn't serve every model over every protocol — and some accept plain chat over Anthropic Messages but **400 on a tool schema**, useless to a coding harness. `lulz` ships the probed result:

| model | claude | codex | note |
|---|:--:|:--:|---|
| qwen3.5/3.6/3.7/3.8, minimax-m2.5/m2.7/m3, kimi-k3 | native | bridged | Messages+tools ok; no `/responses` |
| deepseek-v4-flash/pro | native | native | both |
| muse-spark-1.2/1.3 | bridged | native | Responses-only on the current gateway |
| gpt-5.6-luna | bridged | native | Claude uses the Responses bridge |
| grok-4.5 | bridged | native | Claude uses the Responses bridge |
| glm-5…5.3 + glm-5.3-flash | bridged | bridged | Claude composes Messages → Responses → Chat Completions |
| hy3, kimi-k2.x, mimo-*, ox-alpha | bridged | bridged | Claude composes Messages → Responses → Chat Completions |

The *roster* is never hardcoded: `lulz` reads the gateway's `/v1/models` and caches ids at `~/.cache/lulz/models` for 12h. Bare interactive launches bypass the cache. The table only supplies what the endpoint doesn't — protocol support and context window. `--refresh` re-reads on demand; on failure it falls back to stale cache, then stops gating.

Naming an unserved model is caught before the harness starts:

```
$ lulz launch claude -m glm-9-turbo
error `glm-9-turbo` isn't served by this gateway.
  close by: glm-5, glm-5.1, glm-5.2, glm-5.3, glm-5.3-flash
  full list: lulz models
```

`lulz doctor` re-probes each live id and caches the result at `~/.cache/lulz/caps`, overlaying the baseline so the table doesn't rot.

Only verdicts about the **model** are cached: 200 is `ok`, 400/404/422 is `no`. But 401, 403, 429 and dead connections print with their status and stay uncached — an outage, throttle or bad key says nothing about model capability.

A 5xx gets one retry, then must answer for itself: if the retry returns the *same* 5xx **and** that endpoint served some other model in the same run, the gateway was demonstrably up and the route is broken, so it caches as `no` (reported under `gated`). Otherwise it stays uncached. Without that second question a permanently broken route is indistinguishable from a bad moment — which is how `glm-5.3-flash` stayed the default while 500ing every request.
