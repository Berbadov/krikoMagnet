#!/usr/bin/env bun
import { existsSync, mkdirSync, mkdtempSync, rmSync, unlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

type QueryRow = {
  id: string;
  query: string;
  language?: string;
  topics?: string[];
  relevant?: string[];
  judged?: boolean;
  notes?: string;
};

type EvalRow = {
  id: string;
  query: string;
  language: string;
  judged: boolean;
  topics: string[];
  sites: string[];
  sources_with_results: number;
  result_domains: number;
  results: number;
  urls: string[];
  flags: number[];
  hit: number;
  precision: number;
  ndcg: number;
  domain_hit: number;
  elapsed_ms: number;
  fetches: number;
  bytes: number;
  robots_skips: number;
  blocks: number;
  short_extractions: number;
  error?: string;
  cost_usd: number;
};

const ROOT = resolve(import.meta.dir, "..");
const QUERIES_PATH = join(import.meta.dir, "queries.jsonl");
const DEFAULT_BIN = join(
  ROOT,
  "target",
  "release",
  process.platform === "win32" ? "magnet.exe" : "magnet",
);
const DEEPSEEK_URL = "https://api.deepseek.com/v1/tools/search";
const LINKUP_URL = "https://api.linkup.so/v1/search";
const DEEPSEEK_COST = 0.75 / 1000;
const LINKUP_COST = 0.006;

function parseArgs(argv: string[]) {
  const opts: Record<string, string | boolean> = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i]!;
    if (!a.startsWith("--")) continue;
    const key = a.slice(2);
    const next = argv[i + 1];
    if (next && !next.startsWith("--")) {
      opts[key] = next;
      i++;
    } else {
      opts[key] = true;
    }
  }
  return opts;
}

function normalizeUrl(raw: string): string {
  try {
    const u = new URL(raw.trim());
    let path = decodeURIComponent(u.pathname || "/");
    if (path !== "/" && path.endsWith("/")) path = path.slice(0, -1);
    const scheme = u.protocol === "http:" || u.protocol === "https:" ? "https:" : u.protocol;
    let host = u.hostname.toLowerCase();
    if (host.startsWith("www.")) host = host.slice(4);
    const port = u.port ? `:${u.port}` : "";
    return `${scheme}//${host}${port}${path}${u.search}`;
  } catch {
    return raw.trim();
  }
}

function domainOf(raw: string): string {
  try {
    const host = new URL(raw).hostname.toLowerCase();
    return host.startsWith("www.") ? host.slice(4) : host;
  } catch {
    return "";
  }
}

function ndcg(flags: number[], nRelevant: number, k: number): number {
  if (!nRelevant) return 0;
  let dcg = 0;
  for (let i = 0; i < Math.min(flags.length, k); i++) {
    dcg += (flags[i] ?? 0) / Math.log2(i + 2);
  }
  let ideal = 0;
  for (let i = 0; i < Math.min(k, nRelevant); i++) {
    ideal += 1 / Math.log2(i + 2);
  }
  return ideal ? dcg / ideal : 0;
}

function median(nums: number[]): number {
  if (!nums.length) return 0;
  const s = [...nums].sort((a, b) => a - b);
  const mid = Math.floor(s.length / 2);
  return s.length % 2 === 1 ? s[mid]! : (s[mid - 1]! + s[mid]!) / 2;
}

function pct(nums: number[], p: number): number {
  if (!nums.length) return 0;
  const s = [...nums].sort((a, b) => a - b);
  const idx = Math.min(s.length - 1, Math.round((p / 100) * (s.length - 1)));
  return s[idx]!;
}

function mean(nums: number[]): number {
  return nums.length ? nums.reduce((a, b) => a + b, 0) / nums.length : 0;
}

async function loadQueries(limit: number): Promise<QueryRow[]> {
  const txt = await Bun.file(QUERIES_PATH).text();
  const rows = txt
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter(Boolean)
    .map((l) => JSON.parse(l) as QueryRow);
  return limit > 0 ? rows.slice(0, limit) : rows;
}

async function startServer(
  bin: string,
  cachePath: string,
  fetchCachePath: string,
  budgetMs: number,
  indexPath: string | null,
) {
  const args = [
    "serve",
    "--port",
    "0",
    "--cache",
    cachePath,
    "--fetch-cache",
    fetchCachePath,
    "--budget-ms",
    String(budgetMs),
  ];
  if (indexPath === null) {
    args.push("--no-index");
  } else {
    args.push("--index", indexPath);
  }

  const proc = Bun.spawn([bin, ...args], {
    cwd: ROOT,
    stdout: "ignore",
    stderr: "pipe",
  });

  const reader = proc.stderr.getReader();
  const dec = new TextDecoder();
  let buf = "";
  let baseUrl = "";
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    const { value, done } = await reader.read();
    if (done) break;
    buf += dec.decode(value, { stream: true });
    const m = buf.match(/listening on (http:\/\/[^\s]+)/);
    if (m) {
      baseUrl = m[1]!;
      break;
    }
  }
  if (!baseUrl) {
    reader.releaseLock();
    proc.kill();
    throw new Error(`magnet serve failed to start:\n${buf}`);
  }
  void (async () => {
    try {
      while (!(await reader.read()).done) {}
    } catch {}
  })();
  return {
    baseUrl,
    stop: async () => {
      proc.kill();
      await proc.exited;
    },
  };
}

function scoreResults(q: QueryRow, urls: string[], k: number) {
  const relevant = new Set((q.relevant ?? []).map(normalizeUrl));
  const flags = urls.slice(0, k).map((u) => (relevant.has(normalizeUrl(u)) ? 1 : 0));
  const relDomains = new Set([...relevant].map(domainOf));
  const gotDomains = new Set(urls.slice(0, k).map(domainOf));
  const domainHit = [...relDomains].some((d) => d && gotDomains.has(d)) ? 1 : 0;
  return {
    flags,
    hit: flags.some(Boolean) ? 1 : 0,
    precision: k ? flags.reduce((a, b) => a + b, 0) / k : 0,
    ndcg: ndcg(flags, relevant.size, k),
    domain_hit: domainHit,
  };
}

async function runMagnetPass(baseUrl: string, queries: QueryRow[], k: number, verbose: boolean): Promise<EvalRow[]> {
  const rows: EvalRow[] = [];
  for (let i = 0; i < queries.length; i++) {
    const q = queries[i]!;
    const t0 = performance.now();
    let payload: any = null;
    let error: string | undefined;
    try {
      const resp = await fetch(`${baseUrl}/search`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ query: q.query, k }),
      });
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      payload = await resp.json();
    } catch (e: any) {
      error = String(e?.message ?? e).slice(0, 200);
    }
    const ms = performance.now() - t0;
    const results: any[] = payload?.results ?? [];
    const urls = results.map((r) => String(r.url ?? ""));
    const scored = scoreResults(q, urls, k);
    const stats = payload?.fetch_stats ?? {};
    const plan = payload?.plan ?? {};
    const blocks = results.filter((r) =>
      [401, 403, 429, "robots", "robots-denied"].includes(r.status),
    ).length;
    const shortExt = results.filter((r) => String(r.clean_markdown ?? "").length < 300).length;
    const row: EvalRow = {
      id: q.id,
      query: q.query,
      language: q.language ?? "",
      judged: q.judged !== false && (q.relevant?.length ?? 0) > 0,
      topics: plan.topics ?? q.topics ?? [],
      sites: plan.sites ?? [],
      sources_with_results: Number(plan.sources_with_results ?? 0),
      result_domains: new Set(results.map((r) => r.domain).filter(Boolean)).size,
      results: results.length,
      urls,
      ...scored,
      elapsed_ms: ms,
      fetches: Number(stats.total_fetches ?? 0),
      bytes: Number(stats.total_bytes ?? 0),
      robots_skips: Number(stats.robots_skips ?? 0),
      blocks,
      short_extractions: shortExt,
      error,
      cost_usd: 0,
    };
    rows.push(row);
    if (verbose) {
      const mark = row.hit ? "HIT " : "miss";
      console.log(
        `[${String(i + 1).padStart(2, "0")}/${queries.length}] ${mark} nDCG=${row.ndcg.toFixed(3)} ${ms.toFixed(0).padStart(6)}ms fetch=${String(row.fetches).padStart(2)} ${q.query}`,
      );
    }
  }
  return rows;
}

async function postJsonWithRetry(url: string, headers: Record<string, string>, body: unknown, timeoutMs: number) {
  let lastErr = "";
  for (let attempt = 0; attempt < 3; attempt++) {
    try {
      const resp = await fetch(url, {
        method: "POST",
        headers,
        body: JSON.stringify(body),
        signal: AbortSignal.timeout(timeoutMs),
      });
      if ([429, 500, 502, 503, 504].includes(resp.status)) {
        lastErr = `HTTP ${resp.status}`;
        await Bun.sleep(1500 * (attempt + 1));
        continue;
      }
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      return { data: await resp.json(), error: undefined };
    } catch (e: any) {
      lastErr = String(e?.message ?? e).slice(0, 200);
      await Bun.sleep(1000 * (attempt + 1));
    }
  }
  return { data: null, error: lastErr };
}

async function runPaidProvider(provider: "deepseek" | "linkup", queries: QueryRow[], k: number): Promise<EvalRow[]> {
  const envKey = provider === "deepseek" ? "DEEPSEEK_API_KEY" : "LINKUP_API_KEY";
  const key = process.env[envKey];
  if (!key) throw new Error(`${envKey} is not set`);
  const endpoint = provider === "deepseek" ? DEEPSEEK_URL : LINKUP_URL;
  const cost = provider === "deepseek" ? DEEPSEEK_COST : LINKUP_COST;

  const rows: EvalRow[] = [];
  for (let i = 0; i < queries.length; i++) {
    const q = queries[i]!;
    const body =
      provider === "deepseek"
        ? { query: q.query, count: k, need_content: true }
        : { q: q.query, depth: "standard", outputType: "searchResults" };
    const t0 = performance.now();
    const { data, error } = await postJsonWithRetry(
      endpoint,
      { Authorization: `Bearer ${key}`, "Content-Type": "application/json" },
      body,
      120_000,
    );
    const ms = performance.now() - t0;
    const rawResults: any[] = Array.isArray(data?.results) ? data.results : Array.isArray(data) ? data : [];
    const urls = rawResults.map((r) => String(r.url ?? ""));
    const scored = scoreResults(q, urls, k);
    const row: EvalRow = {
      id: q.id,
      query: q.query,
      language: q.language ?? "",
      judged: q.judged !== false && (q.relevant?.length ?? 0) > 0,
      topics: q.topics ?? [],
      sites: [],
      sources_with_results: 0,
      result_domains: new Set(urls.map(domainOf).filter(Boolean)).size,
      results: rawResults.length,
      urls,
      ...scored,
      elapsed_ms: ms,
      fetches: 0,
      bytes: 0,
      robots_skips: 0,
      blocks: 0,
      short_extractions: 0,
      error,
      cost_usd: error || !rawResults.length ? 0 : cost,
    };
    rows.push(row);
    console.log(`  ${provider.padEnd(8)} [${String(i + 1).padStart(2, "0")}/${queries.length}] ${q.id} hit=${row.hit} ndcg=${row.ndcg.toFixed(2)} ${ms.toFixed(0)}ms`);
  }
  return rows;
}

function summarize(rows: EvalRow[], label: string) {
  const judged = rows.filter((r) => r.judged);
  const okLat = rows.filter((r) => !r.error).map((r) => r.elapsed_ms);
  const totalRes = judged.reduce((a, r) => a + r.results, 0);
  const totalBlocks = judged.reduce((a, r) => a + r.blocks, 0);
  return {
    label,
    n: judged.length,
    errors: rows.filter((r) => Boolean(r.error)).length,
    empty: rows.filter((r) => r.results === 0).length,
    "hit@5": mean(judged.map((r) => r.hit)),
    "ndcg@5": mean(judged.map((r) => r.ndcg)),
    "p@5": mean(judged.map((r) => r.precision)),
    "domain_hit@5": mean(judged.map((r) => r.domain_hit)),
    sources_mean: mean(judged.map((r) => r.sources_with_results)),
    domains_mean: mean(judged.map((r) => r.result_domains)),
    p50_ms: median(okLat),
    p95_ms: pct(okLat, 95),
    fetches_mean: mean(judged.map((r) => r.fetches)),
    fetches_total: judged.reduce((a, r) => a + r.fetches, 0),
    robots_skips: judged.reduce((a, r) => a + r.robots_skips, 0),
    block_rate: totalRes ? totalBlocks / totalRes : 0,
    cost_usd: rows.reduce((a, r) => a + r.cost_usd, 0),
  };
}

function deleteSqlite(path: string) {
  for (const s of ["", "-wal", "-shm"]) {
    if (existsSync(path + s)) unlinkSync(path + s);
  }
}

async function main() {
  const opts = parseArgs(Bun.argv.slice(2));
  const k = Number(opts.k ?? 5);
  const limit = Number(opts.limit ?? 0);
  const bin = String(opts.bin ?? DEFAULT_BIN);
  if (!existsSync(bin)) {
    throw new Error(`binary not found: ${bin} (run: cargo build --release)`);
  }

  const queries = await loadQueries(limit);
  const providersOpt = opts.providers ? String(opts.providers) : "";
  const indexPath = opts["no-index"] ? null : String(opts.index ?? join(ROOT, "data", "site_index.sqlite"));

  if (providersOpt) {
    const providers = providersOpt === "all" ? ["magnet", "deepseek", "linkup"] : providersOpt.split(",").map((s) => s.trim()).filter(Boolean);
    const budgetMs = Number(opts["budget-ms"] ?? 15_000);
    console.log(`${queries.length} queries, top-${k}, providers=${providers.join(",")}, budget=${budgetMs}ms`);
    if (opts["dry-run"]) {
      const est = queries.length * ((providers.includes("deepseek") ? DEEPSEEK_COST : 0) + (providers.includes("linkup") ? LINKUP_COST : 0));
      console.log(`estimated spend: $${est.toFixed(2)} (no requests sent)`);
      return;
    }

    const allRows: Record<string, EvalRow[]> = {};
    if (providers.includes("magnet")) {
      const tmp = mkdtempSync(join(tmpdir(), "magnet-bench-"));
      try {
        const srv = await startServer(bin, join(tmp, "pipe.sqlite"), join(tmp, "fetch.sqlite"), budgetMs, indexPath);
        try {
          allRows.magnet = await runMagnetPass(srv.baseUrl, queries, k, false);
        } finally {
          await srv.stop();
        }
      } finally {
        rmSync(tmp, { recursive: true, force: true });
      }
    }
    if (providers.includes("deepseek")) allRows.deepseek = await runPaidProvider("deepseek", queries, k);
    if (providers.includes("linkup")) allRows.linkup = await runPaidProvider("linkup", queries, k);

    const summaries = providers.map((p) => summarize(allRows[p]!, p));
    console.log("\n| Provider | hit@5 | nDCG@5 | P@5 | domain-hit@5 | p50 | p95 | cost |");
    console.log("|---|---|---|---|---|---|---|---|");
    for (const s of summaries) {
      console.log(`| ${s.label} | ${(s["hit@5"] * 100).toFixed(1)}% | ${s["ndcg@5"].toFixed(3)} | ${s["p@5"].toFixed(2)} | ${(s["domain_hit@5"] * 100).toFixed(1)}% | ${s.p50_ms.toFixed(0)} ms | ${s.p95_ms.toFixed(0)} ms | $${s.cost_usd.toFixed(4)} |`);
    }
    if (opts.json) await Bun.write(String(opts.json), JSON.stringify({ k, summaries, rows: allRows }, null, 1));
    return;
  }

  const budgetMs = Number(opts["budget-ms"] ?? 0);
  const cachePath = join(ROOT, "data", "eval_cache.sqlite");
  const fetchCachePath = join(ROOT, "data", "eval_fetch.sqlite");
  mkdirSync(join(ROOT, "data"), { recursive: true });
  if (opts.fresh) {
    deleteSqlite(cachePath);
    deleteSqlite(fetchCachePath);
  }
  const srv = await startServer(bin, cachePath, fetchCachePath, budgetMs, indexPath);
  try {
    const health = (await (await fetch(`${srv.baseUrl}/health`)).json()) as any;
    const idxInfo = health?.index;
    if (idxInfo && !idxInfo.missing) {
      console.log(`local index: ${idxInfo.pages} pages / ${idxInfo.sites} sites`);
    } else if (indexPath) {
      console.log(`local index: missing (${indexPath})`);
    }
    console.log(`\nCOLD RUN (${queries.length} queries, k=${k}, budget=${budgetMs}ms)`);
    const coldRows = await runMagnetPass(srv.baseUrl, queries, k, true);
    const cold = summarize(coldRows, "cold");

    await fetch(`${srv.baseUrl}/cache/clear-queries`, { method: "POST" });
    console.log("\nWARM-FETCH, COLD-QUERY RUN");
    const warmFetchRows = await runMagnetPass(srv.baseUrl, queries, k, false);
    const warmFetch = summarize(warmFetchRows, "warm_fetch");

    console.log("\nWARM QUERY-CACHE RUN");
    const warmRows = await runMagnetPass(srv.baseUrl, queries, k, false);
    const warm = summarize(warmRows, "warm");

    const md = [
      "| Metric | Value |",
      "|---|---|",
      `| Judged queries | ${cold.n} |`,
      `| Hit@5 | ${(cold["hit@5"] * 100).toFixed(1)}% |`,
      `| nDCG@5 | ${cold["ndcg@5"].toFixed(3)} |`,
      `| Precision@5 | ${cold["p@5"].toFixed(3)} |`,
      `| Cold p50 / p95 | ${cold.p50_ms.toFixed(0)} / ${cold.p95_ms.toFixed(0)} ms |`,
      `| Warm-fetch p50 / p95 | ${warmFetch.p50_ms.toFixed(0)} / ${warmFetch.p95_ms.toFixed(0)} ms |`,
      `| Warm cache p50 / p95 | ${warm.p50_ms.toFixed(0)} / ${warm.p95_ms.toFixed(0)} ms |`,
      `| Fetches / query (cold) | ${cold.fetches_mean.toFixed(1)} (total ${cold.fetches_total}) |`,
      `| Robots skips / block rate | ${cold.robots_skips} / ${(cold.block_rate * 100).toFixed(1)}% |`,
    ].join("\n");
    console.log("\n" + md);

    const misses = coldRows.filter((r) => r.judged && !r.hit);
    if (misses.length) {
      console.log(`\nMisses (${misses.length}):`);
      for (const m of misses) console.log(`  ${m.id} ${m.query} -> ${m.urls.slice(0, 3).join(", ")}`);
    }
    if (opts.json) await Bun.write(String(opts.json), JSON.stringify({ summary: { cold, warmFetch, warm }, cold: coldRows }, null, 1));
    if (opts.markdown) await Bun.write(String(opts.markdown), `# Kriko Magnet evaluation\n\n${md}\n`);
  } finally {
    await srv.stop();
  }
}

await main();
