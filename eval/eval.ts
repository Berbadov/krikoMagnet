#!/usr/bin/env bun
import { existsSync, mkdirSync, mkdtempSync, rmSync, statSync, unlinkSync } from "node:fs";
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
  hit1: number;
  hit3: number;
  hit: number;
  mrr: number;
  precision: number;
  ndcg1: number;
  ndcg3: number;
  ndcg: number;
  domain_hit: number;
  elapsed_ms: number;
  fetches: number;
  bytes: number;
  robots_skips: number;
  blocks: number;
  short_extractions: number;
  median_chars: number;
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

const OPEN_DOMAIN_QUERIES = [
  "kolmogorov complexity",
  "rust async pin unpin future",
  "beta binomial conjugate prior",
  "state space models structured matrices",
  "mitochondrial dna maternal inheritance",
  "maxwell boltzmann distribution derivation",
];

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

function stddev(nums: number[]): number {
  if (nums.length < 2) return 0;
  const m = mean(nums);
  const v = nums.reduce((a, x) => a + (x - m) ** 2, 0) / nums.length;
  return Math.sqrt(v);
}

/** Deterministic seeded 95% bootstrap confidence interval over B=1000 resamples. */
function bootstrapCI(vals: number[], iters = 1000): [number, number] {
  if (!vals.length) return [0, 0];
  let seed = 0x5eed1234;
  const rand = () => {
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
  const n = vals.length;
  const means = new Array<number>(iters);
  for (let b = 0; b < iters; b++) {
    let sum = 0;
    for (let i = 0; i < n; i++) {
      sum += vals[Math.floor(rand() * n)]!;
    }
    means[b] = sum / n;
  }
  means.sort((a, b) => a - b);
  return [means[Math.floor(iters * 0.025)]!, means[Math.floor(iters * 0.975)]!];
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
  const firstIdx = flags.findIndex(Boolean);
  const mrr = firstIdx >= 0 ? 1 / (firstIdx + 1) : 0;
  const relDomains = new Set([...relevant].map(domainOf));
  const gotDomains = new Set(urls.slice(0, k).map(domainOf));
  const domainHit = [...relDomains].some((d) => d && gotDomains.has(d)) ? 1 : 0;
  return {
    flags,
    hit1: flags.slice(0, 1).some(Boolean) ? 1 : 0,
    hit3: flags.slice(0, 3).some(Boolean) ? 1 : 0,
    hit: flags.some(Boolean) ? 1 : 0,
    mrr,
    precision: k ? flags.reduce((a, b) => a + b, 0) / k : 0,
    ndcg1: ndcg(flags, relevant.size, 1),
    ndcg3: ndcg(flags, relevant.size, 3),
    ndcg: ndcg(flags, relevant.size, k),
    domain_hit: domainHit,
  };
}

async function runMagnetPass(
  baseUrl: string,
  queries: QueryRow[],
  k: number,
  verbose: boolean,
  fetchPages = true,
): Promise<EvalRow[]> {
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
        body: JSON.stringify({ query: q.query, k, fetch: fetchPages }),
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
    const okLens = results
      .filter((r) => r.status === 200)
      .map((r) => String(r.clean_markdown ?? "").length);
    const shortExt = okLens.filter((len) => len < 300).length;
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
      median_chars: median(okLens),
      error,
      cost_usd: 0,
    };
    rows.push(row);
    if (verbose) {
      const mark = row.hit ? "HIT " : "miss";
      console.log(
        `[${String(i + 1).padStart(2, "0")}/${queries.length}] ${mark} nDCG=${row.ndcg.toFixed(3)} MRR=${row.mrr.toFixed(2)} ${ms.toFixed(0).padStart(6)}ms fetch=${String(row.fetches).padStart(2)} ${q.query}`,
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
    const lens = rawResults.map((r) => String(r.content ?? r.snippet ?? "").length).filter((n) => n > 0);
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
      median_chars: median(lens),
      error,
      cost_usd: error || !rawResults.length ? 0 : cost,
    };
    rows.push(row);
    console.log(`  ${provider.padEnd(8)} [${String(i + 1).padStart(2, "0")}/${queries.length}] ${q.id} hit=${row.hit} mrr=${row.mrr.toFixed(2)} ndcg=${row.ndcg.toFixed(2)} ${ms.toFixed(0)}ms`);
  }
  return rows;
}

function summarize(rows: EvalRow[], label: string) {
  const judged = rows.filter((r) => r.judged);
  const okLat = rows.filter((r) => !r.error).map((r) => r.elapsed_ms);
  const totalRes = judged.reduce((a, r) => a + r.results, 0);
  const totalBlocks = judged.reduce((a, r) => a + r.blocks, 0);
  const totalShort = judged.reduce((a, r) => a + r.short_extractions, 0);
  const hits = judged.map((r) => r.hit);
  const mrrs = judged.map((r) => r.mrr);
  const ndcgs = judged.map((r) => r.ndcg);
  const meanMs = mean(okLat);
  const totalCost = rows.reduce((a, r) => a + r.cost_usd, 0);
  const hitCount = hits.reduce((a, b) => a + b, 0);
  const chars = judged.map((r) => r.median_chars).filter((x) => x > 0);
  return {
    label,
    n: judged.length,
    errors: rows.filter((r) => Boolean(r.error)).length,
    empty: rows.filter((r) => r.results === 0).length,
    "hit@1": mean(judged.map((r) => r.hit1)),
    "hit@3": mean(judged.map((r) => r.hit3)),
    "hit@5": mean(hits),
    "hit@5_ci": bootstrapCI(hits),
    "mrr@5": mean(mrrs),
    "mrr@5_ci": bootstrapCI(mrrs),
    "ndcg@1": mean(judged.map((r) => r.ndcg1)),
    "ndcg@3": mean(judged.map((r) => r.ndcg3)),
    "ndcg@5": mean(ndcgs),
    "ndcg@5_ci": bootstrapCI(ndcgs),
    "p@5": mean(judged.map((r) => r.precision)),
    "domain_hit@5": mean(judged.map((r) => r.domain_hit)),
    sources_mean: mean(judged.map((r) => r.sources_with_results)),
    domains_mean: mean(judged.map((r) => r.result_domains)),
    min_ms: okLat.length ? Math.min(...okLat) : 0,
    mean_ms: meanMs,
    std_ms: stddev(okLat),
    p50_ms: median(okLat),
    p90_ms: pct(okLat, 90),
    p95_ms: pct(okLat, 95),
    p99_ms: pct(okLat, 99),
    max_ms: okLat.length ? Math.max(...okLat) : 0,
    qps: meanMs > 0 ? 1000 / meanMs : 0,
    fetches_mean: mean(judged.map((r) => r.fetches)),
    fetches_total: judged.reduce((a, r) => a + r.fetches, 0),
    kb_mean: mean(judged.map((r) => r.bytes)) / 1024,
    robots_skips: judged.reduce((a, r) => a + r.robots_skips, 0),
    block_rate: totalRes ? totalBlocks / totalRes : 0,
    short_extraction_rate: totalRes ? totalShort / totalRes : 0,
    median_chars: median(chars),
    cost_usd: totalCost,
    cost_per_1k: rows.length ? (totalCost / rows.length) * 1000 : 0,
    cost_per_hit: hitCount ? totalCost / hitCount : 0,
  };
}

function formatCI(val: number, ci: [number, number], pctMode = false): string {
  if (pctMode) {
    return `${(val * 100).toFixed(1)}% [${(ci[0] * 100).toFixed(1)}–${(ci[1] * 100).toFixed(1)}%]`;
  }
  return `${val.toFixed(3)} [${ci[0].toFixed(3)}–${ci[1].toFixed(3)}]`;
}

function stratifiedMarkdown(rows: EvalRow[], queries: QueryRow[]): string {
  const qById = new Map(queries.map((q) => [q.id, q]));
  const judged = rows.filter((r) => r.judged);

  const byLang = new Map<string, EvalRow[]>();
  for (const r of judged) {
    const lang = qById.get(r.id)?.language || r.language || "unknown";
    if (!byLang.has(lang)) byLang.set(lang, []);
    byLang.get(lang)!.push(r);
  }
  const lines: string[] = [
    "### Stratified Breakdown by Language",
    "| Language | N | Hit@1 | Hit@5 | MRR@5 | nDCG@5 | p50 (ms) | p95 (ms) |",
    "|---|---:|---:|---:|---:|---:|---:|---:|",
  ];
  for (const [lang, list] of [...byLang.entries()].sort()) {
    const s = summarize(list, lang);
    lines.push(
      `| ${lang} | ${s.n} | ${(s["hit@1"] * 100).toFixed(1)}% | ${(s["hit@5"] * 100).toFixed(1)}% | ${s["mrr@5"].toFixed(3)} | ${s["ndcg@5"].toFixed(3)} | ${s.p50_ms.toFixed(0)} | ${s.p95_ms.toFixed(0)} |`,
    );
  }

  const byTopic = new Map<string, EvalRow[]>();
  for (const r of judged) {
    const topics = qById.get(r.id)?.topics ?? r.topics ?? [];
    for (const t of topics) {
      if (!byTopic.has(t)) byTopic.set(t, []);
      byTopic.get(t)!.push(r);
    }
  }
  lines.push(
    "",
    "### Stratified Breakdown by Topic",
    "| Topic | N | Hit@5 | MRR@5 | nDCG@5 | p50 (ms) |",
    "|---|---:|---:|---:|---:|---:|",
  );
  for (const [topic, list] of [...byTopic.entries()].sort((a, b) => b[1].length - a[1].length)) {
    const s = summarize(list, topic);
    lines.push(
      `| ${topic} | ${s.n} | ${(s["hit@5"] * 100).toFixed(1)}% | ${s["mrr@5"].toFixed(3)} | ${s["ndcg@5"].toFixed(3)} | ${s.p50_ms.toFixed(0)} |`,
    );
  }
  return lines.join("\n");
}

function pairedComparisonMarkdown(baseName: string, baseRows: EvalRow[], otherName: string, otherRows: EvalRow[]): string {
  const otherMap = new Map(otherRows.map((r) => [r.id, r]));
  let winN = 0, tieN = 0, lossN = 0;
  let winH = 0, tieH = 0, lossH = 0;
  for (const b of baseRows) {
    if (!b.judged) continue;
    const o = otherMap.get(b.id);
    if (!o) continue;
    if (b.ndcg > o.ndcg + 1e-4) winN++;
    else if (o.ndcg > b.ndcg + 1e-4) lossN++;
    else tieN++;
    if (b.hit > o.hit) winH++;
    else if (o.hit > b.hit) lossH++;
    else tieH++;
  }
  return `| ${baseName} vs ${otherName} | Hit@5 W/T/L: ${winH}/${tieH}/${lossH} | nDCG@5 W/T/L: ${winN}/${tieN}/${lossN} |`;
}

async function runSubsystems(bin: string, baseUrl: string, queries: QueryRow[], indexPath: string | null) {
  const mb = (p: string) => (existsSync(p) ? statSync(p).size / (1024 * 1024) : 0);
  const footprint = {
    binary_mb: Number(mb(bin).toFixed(2)),
    index_mb: Number((indexPath ? mb(indexPath) : 0).toFixed(2)),
    sources_trust_kb: Number((mb(join(ROOT, "data", "sources.sqlite")) * 1024).toFixed(1)),
  };

  // 1. Discovery + In-Process Ranker speed (fetch=false), splitting pure local SQLite queries vs all queries
  const idxPass = await runMagnetPass(baseUrl, queries, 5, false, false);
  const idxLat = idxPass.map((r) => r.elapsed_ms);
  const localLat = idxPass.filter((r) => r.fetches === 0).map((r) => r.elapsed_ms);
  const indexBench = {
    queries: queries.length,
    local_only_n: localLat.length,
    local_p50_ms: Number(median(localLat).toFixed(2)),
    local_p95_ms: Number(pct(localLat, 95).toFixed(2)),
    local_qps: Number((1000 / Math.max(mean(localLat), 0.01)).toFixed(1)),
    mean_ms: Number(mean(idxLat).toFixed(2)),
    p50_ms: Number(median(idxLat).toFixed(2)),
    p95_ms: Number(pct(idxLat, 95).toFixed(2)),
    p99_ms: Number(pct(idxLat, 99).toFixed(2)),
    qps: Number((1000 / Math.max(mean(idxLat), 0.01)).toFixed(1)),
  };

  // 2. Probabilistic Calibration benchmark over cached golden hosts
  const calOut = Bun.spawnSync([bin, "calibrate", "--json"], { cwd: ROOT, stdout: "pipe", stderr: "pipe" });
  let calibSummary: any = null;
  if (calOut.exitCode === 0) {
    const cal = JSON.parse(new TextDecoder().decode(calOut.stdout));
    const hosts: any[] = cal.hosts ?? [];
    calibSummary = {
      hosts: hosts.length,
      pages: cal.baseline?.pages_calibrated ?? 0,
      promoted: hosts.filter((h) => h.decision === "promote").length,
      uncertain: hosts.filter((h) => h.decision === "uncertain").length,
      rejected: hosts.filter((h) => h.decision === "reject").length,
      median_p_golden: Number(median(hosts.map((h) => h.p_golden)).toFixed(3)),
    };
  }

  // 3. Domain-agnostic open research/technical probe
  const openRows: any[] = [];
  for (const q of OPEN_DOMAIN_QUERIES) {
    const t0 = performance.now();
    const resp = await fetch(`${baseUrl}/search`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ query: q, k: 3, fetch: true }),
    });
    const payload: any = resp.ok ? await resp.json() : {};
    const ms = performance.now() - t0;
    const res: any[] = payload?.results ?? [];
    const ok = res.filter((r) => r.status === 200);
    openRows.push({
      query: q,
      ms: Number(ms.toFixed(0)),
      results: res.length,
      http200: ok.length,
      median_chars: Math.round(median(ok.map((r) => String(r.clean_markdown ?? "").length))),
      top_domains: res.map((r) => r.domain).slice(0, 3),
    });
  }

  // 4. Autonomous live Scout benchmark on unseen heterogeneous sites (read-only, no registry mutation)
  const scoutProc = Bun.spawnSync(
    [bin, "scout", "docs.rs", "simonwillison.net", "old.reddit.com", "--sample", "5", "--json"],
    { cwd: ROOT, stdout: "pipe", stderr: "pipe" },
  );
  let scoutRows: any[] = [];
  if (scoutProc.exitCode === 0) {
    const raw = JSON.parse(new TextDecoder().decode(scoutProc.stdout));
    scoutRows = (raw ?? []).map((r: any) => ({
      domain: r.domain,
      method: r.discovered_method,
      pages: r.estimate?.pages_sampled ?? 0,
      p_golden: Number((r.estimate?.p_golden ?? 0).toFixed(3)),
      ci: `[${Number(r.estimate?.p_low ?? 0).toFixed(2)}, ${Number(r.estimate?.p_high ?? 0).toFixed(2)}]`,
      action: r.action,
      reason: r.reason,
    }));
  }

  return { footprint, indexBench, calibSummary, openRows, scoutRows };
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
  const proMode = Boolean(opts.pro);
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
          await fetch(`${srv.baseUrl}/cache/clear-queries`, { method: "POST" });
          allRows.magnet_warm_fetch = await runMagnetPass(srv.baseUrl, queries, k, false);
          allRows.magnet_warm_cache = await runMagnetPass(srv.baseUrl, queries, k, false);
        } finally {
          await srv.stop();
        }
      } finally {
        rmSync(tmp, { recursive: true, force: true });
      }
    }
    if (providers.includes("deepseek")) allRows.deepseek = await runPaidProvider("deepseek", queries, k);
    if (providers.includes("linkup")) allRows.linkup = await runPaidProvider("linkup", queries, k);

    const orderedKeys = [
      ...(providers.includes("magnet") ? ["magnet", "magnet_warm_fetch", "magnet_warm_cache"] : []),
      ...providers.filter((p) => p !== "magnet"),
    ];
    const summaries = orderedKeys.filter((k) => allRows[k]).map((k) => summarize(allRows[k]!, k));
    const lines: string[] = [
      "| Provider | Hit@1 | Hit@5 (95% CI) | MRR@5 | nDCG@5 (95% CI) | Domain-Hit@5 | p50 / p95 / p99 (ms) | QPS | Cost (/1k) |",
      "|---|---:|---|---|---:|---:|---|---:|---:|",
    ];
    for (const s of summaries) {
      lines.push(
        `| ${s.label} | ${(s["hit@1"] * 100).toFixed(1)}% | ${formatCI(s["hit@5"], s["hit@5_ci"], true)} | ${s["mrr@5"].toFixed(3)} | ${formatCI(s["ndcg@5"], s["ndcg@5_ci"])} | ${(s["domain_hit@5"] * 100).toFixed(1)}% | ${s.p50_ms.toFixed(0)} / ${s.p95_ms.toFixed(0)} / ${s.p99_ms.toFixed(0)} | ${s.qps.toFixed(1)} | $${s.cost_usd.toFixed(4)} ($${s.cost_per_1k.toFixed(2)}) |`,
      );
    }
    if (allRows.magnet && (allRows.deepseek || allRows.linkup)) {
      lines.push("", "### Paired Query Comparison", "| Pair | Hit@5 W/T/L | nDCG@5 W/T/L |", "|---|---|---|");
      if (allRows.deepseek) lines.push(pairedComparisonMarkdown("magnet", allRows.magnet, "deepseek", allRows.deepseek));
      if (allRows.linkup) lines.push(pairedComparisonMarkdown("magnet", allRows.magnet, "linkup", allRows.linkup));
    }
    if (allRows.magnet) {
      lines.push("", stratifiedMarkdown(allRows.magnet, queries));
    }
    const md = lines.join("\n");
    console.log("\n" + md);
    if (opts.json) await Bun.write(String(opts.json), JSON.stringify({ k, summaries, rows: allRows }, null, 1));
    if (opts.markdown) await Bun.write(String(opts.markdown), `# Kriko Magnet Provider Benchmark\n\n${md}\n`);
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

    const subs = proMode ? await runSubsystems(bin, srv.baseUrl, queries, indexPath) : null;

    const mdLines = [
      "| Metric | Value |",
      "|---|---|",
      `| Judged queries | ${cold.n} |`,
      `| Hit@1 / Hit@3 / Hit@5 | ${(cold["hit@1"] * 100).toFixed(1)}% / ${(cold["hit@3"] * 100).toFixed(1)}% / ${formatCI(cold["hit@5"], cold["hit@5_ci"], true)} |`,
      `| MRR@5 (95% CI) | ${formatCI(cold["mrr@5"], cold["mrr@5_ci"])} |`,
      `| nDCG@1 / nDCG@3 / nDCG@5 (95% CI) | ${cold["ndcg@1"].toFixed(3)} / ${cold["ndcg@3"].toFixed(3)} / ${formatCI(cold["ndcg@5"], cold["ndcg@5_ci"])} |`,
      `| Precision@5 / Domain-Hit@5 | ${cold["p@5"].toFixed(3)} / ${(cold["domain_hit@5"] * 100).toFixed(1)}% |`,
      `| Cold latency (p50 / p90 / p95 / p99) | ${cold.p50_ms.toFixed(0)} / ${cold.p90_ms.toFixed(0)} / ${cold.p95_ms.toFixed(0)} / ${cold.p99_ms.toFixed(0)} ms (${cold.qps.toFixed(2)} QPS) |`,
      `| Warm-fetch latency (p50 / p95 / p99) | ${warmFetch.p50_ms.toFixed(0)} / ${warmFetch.p95_ms.toFixed(0)} / ${warmFetch.p99_ms.toFixed(0)} ms (${warmFetch.qps.toFixed(1)} QPS) |`,
      `| Warm cache latency (p50 / p95 / p99) | ${warm.p50_ms.toFixed(1)} / ${warm.p95_ms.toFixed(1)} / ${warm.p99_ms.toFixed(1)} ms (${warm.qps.toFixed(0)} QPS) |`,
      `| Extraction (median chars / <300 short rate) | ${cold.median_chars.toFixed(0)} chars / ${(cold.short_extraction_rate * 100).toFixed(1)}% |`,
      `| Fetches / query & mean KB (cold) | ${cold.fetches_mean.toFixed(1)} fetches (total ${cold.fetches_total}), ${cold.kb_mean.toFixed(0)} KB/q |`,
      `| Robots skips / block rate / cost | ${cold.robots_skips} / ${(cold.block_rate * 100).toFixed(1)}% / $${cold.cost_usd.toFixed(4)} |`,
      "",
      stratifiedMarkdown(coldRows, queries),
    ];

    if (subs) {
      mdLines.push(
        "",
        "### Subsystem & Footprint Benchmarks",
        "| Subsystem | Measurement |",
        "|---|---|",
        `| Binary / FTS5 Index / Trust DB | ${subs.footprint.binary_mb} MB / ${subs.footprint.index_mb} MB / ${subs.footprint.sources_trust_kb} KB |`,
        `| Pure Local Index-First + BM25 (${subs.indexBench.local_only_n}q, 0 HTTP) | p50=${subs.indexBench.local_p50_ms} ms, p95=${subs.indexBench.local_p95_ms} ms (${subs.indexBench.local_qps} QPS) |`,
        `| Discovery + Live JSON APIs (all 60q, no fetch) | p50=${subs.indexBench.p50_ms} ms, p95=${subs.indexBench.p95_ms} ms, p99=${subs.indexBench.p99_ms} ms (${subs.indexBench.qps} QPS) |`,
      );
      if (subs.calibSummary) {
        mdLines.push(
          `| Source Calibration (real cache) | ${subs.calibSummary.hosts} hosts / ${subs.calibSummary.pages} pages, promoted=${subs.calibSummary.promoted}, uncertain=${subs.calibSummary.uncertain}, rejected=${subs.calibSummary.rejected}, median p=${subs.calibSummary.median_p_golden} |`,
        );
      }
      mdLines.push("", "### Cross-Domain General Queries ($0, Polite)", "| Query | Latency (ms) | HTTP 200 | Median Chars | Top Domains |", "|---|---:|---:|---:|---|");
      for (const o of subs.openRows) {
        mdLines.push(`| ${o.query} | ${o.ms} | ${o.http200}/${o.results} | ${o.median_chars} | ${o.top_domains.join(", ")} |`);
      }
      if (subs.scoutRows.length) {
        mdLines.push("", "### Autonomous Scout Live Benchmark (Unseen Sites)", "| Domain | Method | n | p_golden | 95% Credible | Action | Note |", "|---|---|---:|---:|---|---|---|");
        for (const r of subs.scoutRows) {
          mdLines.push(`| ${r.domain} | ${r.method} | ${r.pages} | ${r.p_golden} | ${r.ci} | ${r.action} | ${r.reason} |`);
        }
      }
    }

    const md = mdLines.join("\n");
    console.log("\n" + md);

    const misses = coldRows.filter((r) => r.judged && !r.hit);
    if (misses.length) {
      console.log(`\nMisses (${misses.length}):`);
      for (const m of misses) console.log(`  ${m.id} ${m.query} -> ${m.urls.slice(0, 3).join(", ")}`);
    }
    if (opts.json) {
      await Bun.write(String(opts.json), JSON.stringify({ summary: { cold, warmFetch, warm }, cold: coldRows, subsystems: subs }, null, 1));
    }
    if (opts.markdown) await Bun.write(String(opts.markdown), `# Kriko Magnet Benchmark Report\n\n${md}\n`);
  } finally {
    await srv.stop();
  }
}

await main();
