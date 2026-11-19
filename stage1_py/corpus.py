"""Generate the synthetic 500-document test corpus + 15 judged queries.

Produces data/corpus.jsonl (documents) and data/queries.json (queries with
binary relevance judgments). Relevant docs live on authoritative domains drawn
from the cached Tranco top-100k; SEO-spam and filler docs pollute the pool to
prove the authority + freshness priors matter.
"""
import json
import os
import random
from datetime import datetime, timedelta, timezone

from tranco_cache import load_top100k

DATA_DIR = os.path.join(os.path.dirname(__file__), "..", "data")
RNG = random.Random(7)

NOW = datetime.now(timezone.utc)


def iso_days_ago(days: float) -> str:
    return (NOW - timedelta(days=days)).isoformat()


def base_domain(url: str) -> str:
    """Registrable-ish domain: last two labels (fine for com/org/net)."""
    from urllib.parse import urlparse
    host = urlparse(url).netloc.lower()
    labels = host.split(".")
    return ".".join(labels[-2:]) if len(labels) >= 2 else host


ABSENT_DOMAINS = {
    "techseohub.net", "bestgadgetdeals.top", "buyguideonline.com",
    "seovault.xyz", "theproductsco.com", "dealmasterz.com",
    "quicktopten.com", "reviewsuite.io", "gadgetsmarts.org",
    "top10findings.com", "wellnessmagic.xyz", "dealseekerz.top",
    "travelhackzz.com", "techreviewhub.net", "shoppydeals.live",
    "boostpicks.com", "rankedreviews.net", "cheapboon.com",
    "findscoop.com", "thebestlist.org",
}

# --- relevant docs per query: (domain, url_path, title, snippet, days_ago) ---
QUERIES = [
    {
        "id": "q1", "category": "product",
        "text": "iphone 14 battery draining fast",
        "relevant": [
            ("reddit.com", "r/apple/comments/x1/battery", "iPhone 14 battery draining fast? Here's what fixed mine",
             "Battery drain on iPhone 14 after iOS update. Background app refresh, location services, and chat 5G were the culprits. Disable those to stop fast drain.", 6),
            ("apple.com", "discussions/thread/battery", "iPhone 14 battery draining quickly - Apple Support Community",
             "Multiple iPhone 14 owners report fast battery drain. Try recalibrating, checking battery health, and updating to the latest iOS to reduce drain.", 12),
            ("xda-developers.com", "how-to/iphone-14-battery-drain-fix", "How to fix iPhone 14 battery drain",
             "Guide to fixing iPhone 14 fast battery drain: turn off Background App Refresh, disable Always-On Display, adjust 5G auto, and find rogue apps.", 20),
            ("reddit.com", "r/iphone/comments/y2/battery", "Battery drain after iOS update on iPhone 14",
             "Discussion of battery draining fast after iOS update on iPhone 14. Users share settings that doubled battery life and stopped fast drain overnight.", 30),
        ],
    },
    {
        "id": "q2", "category": "product",
        "text": "best budget mechanical keyboard 2026",
        "relevant": [
            ("reddit.com", "r/MechanicalKeyboards/comments/bb/B", "Best budget mechanical keyboard 2026 megathread",
             "Community-curated list of the best budget mechanical keyboards in 2026 under $100: gasket mount, hot-swap, south-facing RGB. Vote now.", 4),
            ("rtings.com", "keyboard/reviews/best/budget", "Best Budget Mechanical Keyboards - RTINGS.com",
             "Objectively reviewed budget mechanical keyboards by typing feel, noise, and build quality. Top picks with measured latency and acoustics.", 15),
            ("tomshardware.com", "best-picks/best-budget-mechanical-keyboards", "Best budget mechanical keyboards in 2026",
             "Toms Hardware picks affordable mechanical keyboards that punch above their price: sturdy frames, quiet switches, solid wireless performance.", 25),
            ("wikipedia.org", "wiki/Mechanical_keyboard", "Mechanical keyboard - Wikipedia",
             "Overview of mechanical keyboards, switch types, and layouts. Background reference for shopping for a budget mechanical keyboard.", 200),
        ],
    },
    {
        "id": "q3", "category": "product",
        "text": "rtx 4070 super cooler noise",
        "relevant": [
            ("reddit.com", "r/nvidia/comments/cc/noise", "RTX 4070 Super cooler noise / coil whine",
             "Owners report RTX 4070 Super fan noise and coil whine. Discussion of undervolting, fan curves, and which AIB models run quietest.", 8),
            ("techpowerup.com", "reviews/nvidia/rtx-4070-super", "NVIDIA RTX 4070 Super review - noise & thermals",
             "TPU review measures RTX 4070 Super cooler noise in dBA under load, coil whine behavior, and temperature across reference and custom boards.", 18),
            ("tomshardware.com", "reviews/rtx-4070-super", "RTX 4070 Super review: thermals and noise",
             "Noise-normalized testing of RTX 4070 Super coolers. How to reduce fan noise and coil whine with custom fan curves and undervolting.", 22),
            ("reddit.com", "r/buildapc/comments/dd/4070", "RTX 4070 Super fan noise fix — set a custom curve",
             "Users share fan curve settings to quiet the RTX 4070 Super. Idle fan stop and undervolt profiles reduce audible cooler noise.", 40),
        ],
    },
    {
        "id": "q4", "category": "product",
        "text": "airpods pro 2 static noise fix",
        "relevant": [
            ("reddit.com", "r/airpods/comments/ee/static", "AirPods Pro 2 static noise - fixed!",
             "AirPods Pro 2 producing static noise or crackling. Fix: re-pair, update firmware, disable spatial audio, clean mesh, or get a replacement.", 5),
            ("apple.com", "support/airpods/static", "AirPods Pro 2 static or crackling sounds",
             "Apple guidance for static noise on AirPods Pro 2: check firmware, reset, ensure Bluetooth sources, and how to request service for a faulty unit.", 10),
            ("macrumors.com", "how-to/airpods-pro-2-static-fix", "How to fix AirPods Pro 2 static noise",
             "Step-by-step fixes for AirPods Pro 2 static noise: update to latest firmware, turn off always-on Siri, and clean the microphone meshes.", 28),
            ("theverge.com", "2023/airpods-pro-2", "AirPods Pro 2 review and noise notes",
             "Review coverage of AirPods Pro 2 including occasional static/crackling reports and what users found fixes the noise.", 300),
        ],
    },
    {
        "id": "q5", "category": "tech",
        "text": "sqlite window functions tutorial",
        "relevant": [
            ("sqlite.org", "windowfunctions.html", "SQLite Window Functions - Official Documentation",
             "Official SQLite reference for window functions: OVER, PARTITION BY, ORDER BY, frames, and the full list of built-in window functions.", 3),
            ("sqlitetutorial.net", "sqlite-window-functions", "SQLite Window Functions Tutorial",
             "Hands-on SQLite tutorial explaining how to use window functions with examples: ROW_NUMBER, RANK, LAG, LEAD, and moving averages.", 14),
            ("geeksforgeeks.org", "sqlite-window-functions", "SQLite Window Functions - GeeksforGeeks",
             "Tutorial on SQLite window functions with syntax and worked examples for ranking, running totals, and frame specifications.", 33),
            ("stackoverflow.com", "questions/sqlite-window", "SQLite window functions: OVER PARTITION BY example",
             "Stack Overflow Q&A explaining SQLite window functions with concrete queries, partitioning, ordering, and frame clause pitfalls.", 90),
        ],
    },
    {
        "id": "q6", "category": "tech",
        "text": "python asyncio httpx concurrent requests",
        "relevant": [
            ("python-httpx.org", "async", "Async Support - HTTPX Documentation",
             "HTTPX official docs on async clients with asyncio: httpx.AsyncClient, awaiting requests, and running many concurrent requests with asyncio.gather.", 7),
            ("realpython.com", "async-io-python", "Async IO in Python: A Complete Walkthrough",
             "Real Python guide to asyncio including writing concurrent HTTP requests, tasks, event loops, and avoiding common pitfalls.", 19),
            ("stackoverflow.com", "questions/httpx-async", "How to make concurrent HTTP requests with httpx + asyncio",
             "Stack Overflow answer showing code for concurrent httpx requests with asyncio.gather, client reuse, and connection pool limits.", 45),
            ("docs.python.org", "3/library/asyncio.html", "asyncio — Asynchronous I/O — Python docs",
             "Python standard library documentation for asyncio: coroutines, tasks, gather, and the event loop used for concurrent I/O.", 60),
        ],
    },
    {
        "id": "q7", "category": "tech",
        "text": "nginx location regex priority",
        "relevant": [
            ("nginx.org", "en/docs/http/ngx_http_core_module.html#location", "ngx_http_core_module location directive - NGINX docs",
             "Official NGINX docs describing location matching order: exact =, longest prefix, then regular expressions with ^~ and ~* priority rules.", 9),
            ("digitalocean.com", "community/tutorials/nginx-location-directive", "Understanding Nginx location directive and priority",
             "DigitalOcean tutorial explaining NGINX location block priority: how prefix and regex locations are matched and which type wins first.", 21),
            ("serverfault.com", "questions/nginx-location-priority", "NGINX location priority: regex vs prefix matching",
             "Server Fault thread clarifying how NGINX decides between prefix locations and regular-expression locations in the matching order.", 55),
            ("nginx.org", "en/docs/http/ngx_http_core_module.html#regex", "NGINX docs on regular expression locations",
             "Reference for regex location modifiers ~ and ~* in NGINX and how they are evaluated after the best prefix match is found.", 200),
        ],
    },
    {
        "id": "q8", "category": "tech",
        "text": "postgresql jsonb indexing partial index",
        "relevant": [
            ("postgresql.org", "docs/current/datatype-json.html", "PostgreSQL JSONB data type docs",
             "Official PostgreSQL docs for JSONB including GIN index support, operators, and tips for indexing jsonb columns efficiently.", 11),
            ("postgresql.org", "docs/current/indexes-partial.html", "PostgreSQL Partial Indexes docs",
             "Official docs covering partial indexes (WHERE clause) and when they speed up queries that target a subset of rows.", 11),
            ("cybertec-postgresql.com", "en/postgresql-jsonb-indexing", "PostgreSQL JSONB indexing performance",
             "Cybertec blog on indexing JSONB with GIN vs B-tree, partial indexes on jsonb subfields, and query planning tips.", 30),
            ("stackoverflow.com", "questions/postgres-jsonb-partial-index", "Indexing JSONB with a partial index in PostgreSQL",
             "Stack Overflow answer demonstrating a partial GIN index on a JSONB field and the query patterns it accelerates.", 70),
        ],
    },
    {
        "id": "q9", "category": "general",
        "text": "how to cook perfect scrambled eggs",
        "relevant": [
            ("seriouseats.com", "recipes/how-to-make-perfect-scrambled-eggs", "The Food Lab: How to Make Perfect Scrambled Eggs",
             "Kenji's method for perfect scrambled eggs: low heat, constant stirring, and early removal for creamy, soft curds that never overcook.", 16),
            ("bonappetit.com", "article/how-to-make-scrambled-eggs", "How to Make Scrambled Eggs - Bon Appetit",
             "Bon Appetit technique for fluffy scrambled eggs: cream, low heat, gentle folding, and salt timing for tender curds.", 27),
            ("reddit.com", "r/Cooking/comments/ff/eggs", "Best way to make perfect scrambled eggs?",
             "r/Cooking thread on making perfect scrambled eggs: whisking, butter, low temperature, and the custard-like finish.", 13),
            ("cooking.nytimes.com", "guides/how-to-scrambled-eggs", "How to Make Scrambled Eggs | NYT Cooking",
             "NYT Cooking guide to soft scrambled eggs: gentle heat, constant motion, and serving just before they look done.", 40),
        ],
    },
    {
        "id": "q10", "category": "general",
        "text": "best hiking trails near seattle",
        "relevant": [
            ("reddit.com", "r/Seattle/comments/gg/trails", "Best hiking trails near Seattle - local recommendations",
             "Locals recommend the best hikes near Seattle: Mount Si, Rattlesnake Ledge, Lake 22, and off-the-radar trails with fewer crowds.", 9),
            ("wta.org", "hikes/browse/near-seattle", "Best Hikes near Seattle - Washington Trails Association",
             "WTA's curated list of the best hiking trails near Seattle with difficulty, length, gain, and the current trail conditions.", 24),
            ("alltrails.com", "parks/us/washington/seattle", "Best hiking trails in Seattle, Washington - AllTrails",
             "AllTrails top-rated hikes near Seattle with photos, elevation profiles, and user reviews for routes of every difficulty.", 35),
            ("recreation.gov", "articles/seattle-area-hikes", "Top Seattle-area hikes to plan in 2026",
             "List of scenic trails near Seattle including alpine lakes, waterfalls, and ridge scrambles with permit tips.", 50),
        ],
    },
    {
        "id": "q11", "category": "product",
        "text": "rivian r1s review",
        "relevant": [
            ("caranddriver.com", "reviews/rivian-r1s", "Rivian R1S Review - Car and Driver",
             "Car and Driver tests the Rivian R1S: range, off-road capability, interior space, and how it stacks up against other electric SUVs.", 18),
            ("edmunds.com", "rivian/r1s/review", "Rivian R1S Review - Edmunds",
             "Edmunds review of the Rivian R1S covering driving dynamics, towing, tech, and value across trim levels.", 26),
            ("reddit.com", "r/Rivian/comments/hh/r1s", "Rivian R1S owner review - 10k miles",
             "Long-term owner review of the Rivian R1S: real-world range, build quality, software, and ownership costs.", 12),
            ("motortrend.com", "reviews/rivian-r1s", "Rivian R1S First Test & Review - MotorTrend",
             "MotorTrend's R1S test: acceleration, braking, off-road capability, and verdict on the electric three-row SUV.", 33),
        ],
    },
    {
        "id": "q12", "category": "general",
        "text": "intermittent fasting benefits",
        "relevant": [
            ("healthline.com", "nutrition/intermittent-fasting-guide", "Intermittent Fasting 101 - Healthline",
             "Guide to intermittent fasting: methods, how it works, benefits for weight loss and metabolism, and who should avoid it.", 20),
            ("mayoclinic.org", "healthy-lifestyle/nutrition/in-depth/intermittent-fasting", "Intermittent fasting - Mayo Clinic",
             "Mayo Clinic's overview of intermittent fasting benefits, risks, and what the evidence actually says about eating windows.", 31),
            ("health.harvard.edu", "staying-healthy/intermittent-fasting", "Intermittent fasting: What is it, and how does it work?",
             "Harvard Health explains intermittent fasting's potential benefits for weight, blood sugar, and longevity, plus cautions.", 44),
            ("webmd.com", "diet/intermittent-fasting", "Intermittent Fasting: Benefits and Risks - WebMD",
             "WebMD covers intermittent fasting schedules, what to eat, metabolic benefits, and possible downsides.", 60),
        ],
    },
    {
        "id": "q13", "category": "tech",
        "text": "docker compose volumes vs bind mounts",
        "relevant": [
            ("docs.docker.com", "compose/how-tos/volumes", "Volumes vs bind mounts in Docker Compose - Docker docs",
             "Official Docker docs comparing named volumes and bind mounts in Compose: management, portability, performance, and when to use each.", 8),
            ("digitalocean.com", "community/tutorials/how-to-use-docker-compose-volumes", "How to use volumes and bind mounts with Docker Compose",
             "DigitalOcean tutorial on Docker Compose volumes vs bind mounts with YAML examples and trade-offs for dev vs production.", 23),
            ("stackoverflow.com", "questions/docker-compose-volume-vs-bind", "Docker Compose named volume vs bind mount differences",
             "Stack Overflow answer contrasting named volumes and bind mounts: where data lives, who owns permissions, and backup behavior.", 40),
            ("reddit.com", "r/docker/comments/ii/volumes", "Bind mounts vs volumes: what do you actually use?",
             "r/docker discussion on when to use named volumes over bind mounts for databases, caches, and source code in Compose.", 17),
        ],
    },
    {
        "id": "q14", "category": "product",
        "text": "best android phone camera 2026",
        "relevant": [
            ("gsmarena.com", "best_camera_phones-review-2026", "Best camera phones of 2026 - GSMArena",
             "GSMArena's ranking of the best Android camera phones in 2026 by photo and video quality, zoom, and low-light performance.", 6),
            ("androidauthority.com", "best-android-camera-phones", "Best Android camera phones in 2026",
             "Android Authority picks the best Android phones for photography, covering main sensors, telephoto, ultrawide, and camera software.", 14),
            ("dxomark.com", "smartphones/ranking", "Best smartphone cameras by DxOMark",
             "DxOMark's camera test rankings for Android phones, with sensor, zoom, and video sub-scores for the top models.", 22),
            ("reddit.com", "r/Android/comments/jj/camera", "Best Android phone camera in 2026 - discussion",
             "r/Android users debate the best Android camera phone, sharing real photo samples and comparisons across flagship models.", 9),
        ],
    },
    {
        "id": "q15", "category": "product",
        "text": "home espresso machine under $500",
        "relevant": [
            ("reddit.com", "r/espresso/comments/kk/under500", "Best home espresso machine under $500 - r/espresso",
             "r/espresso recommends the best sub-$500 espresso machines: Gaggia Classic Pro, Bambino Plus, and modding potential.", 7),
            ("theverge.com", "2026/best-espresso-machines", "Best home espresso machines under $500 - The Verge",
             "The Verge's roundup of capable home espresso machines under $500 with steam wand performance and consistency notes.", 28),
            ("nytimes.com", "wirecutter/reviews/best-espresso-machine", "The Best Home Espresso Machine - Wirecutter",
             "Wirecutter's pick for a home espresso machine under $500, with runner-ups and what to look for in a capable machine.", 19),
            ("seriouseats.com", "espresso-machine-reviews-under-500", "Best espresso machines under $500 - Serious Eats",
             "Serious Eats tests affordable espresso machines for pressure, temperature stability, and the quality of shots they pull.", 36),
        ],
    },
]

# --- filler topics: (topic, domains pool style) ---
FILLER_TOPICS = [
    "personal finance budgeting basics",
    "dog training positive reinforcement",
    "meal prep containers for the week",
    "home insulation for winter costs",
    "bicycle chain maintenance and lube",
    "indoor plant watering schedule",
    "car oil change frequency guide",
    "retirement savings 401k plans",
    "running cadence improvement tips",
    "solar panel installation cost 2026",
    "easy dessert recipes with chocolate",
    "resume writing tips for engineers",
    "public speaking anxiety techniques",
    "ergonomic desk setup at home",
    "water filtration systems for tap water",
    "yoga for lower back pain relief",
    "how to start a small garden",
    "best running shoes for wide feet",
    "home security camera installation",
    "protein powder for muscle recovery",
    "how to reduce screen time",
    "efficient study techniques for finals",
    "best ways to invest spare cash",
    "low carb dinner ideas quick",
    "setting up a home office",
]

FILLER_DOMAINS = [
    "besthealthdigest.com", "fitnesspure.net", "homekitpro.com", "moneymoments.org",
    "careercompass.io", "learnfaster.net", "wellnessdaily.com", "kitchencraftz.com",
    "backyardbloom.com", "geekbasics.net", "simplelivinghub.com", "urbangarden.co",
    "dailyproductivity.com", "thecleverhome.com", "smartbudgeting.net", "runbetterclub.com",
    "laptoplabnews.com", "homeofficeguru.com", "studywise.org", "proteinhub.net",
    "cardiochamp.com", "sleepcycleplus.com", "hydrationhub.net", "mindfulmornings.com",
    "booksmartclub.com", "cookingcoop.com", "plantpals.org", "fixithome.net",
    "techjust.net", "theorganizedlife.com", "everydaysavings.com", "outdoorfinds.com",
    "ergonomicease.com", "thewaterwise.com", "powerpractical.com", "campusclever.com",
]


def _filler_docs():
    docs = []
    i = 0
    topics = FILLER_TOPICS[:]
    while len(docs) < 395:
        topic = topics[i % len(topics)]
        dom = RNG.choice(FILLER_DOMAINS)
        idx = i + 1
        title = f"{topic.title()} — practical guide #{idx}"
        url = f"https://{dom}/articles/{topic.replace(' ', '-')}-{idx}"
        days = RNG.uniform(2, 700)
        docs.append({
            "id": f"fill{idx:04d}",
            "url": url,
            "domain": dom,
            "title": title,
            "snippet": f"Tips about {topic}. Readers share what worked and what to avoid. Simple steps for beginners.",
            "published": iso_days_ago(days),
        })
        i += 1
    return docs


def _spam_docs():
    """Generate SEO-spam docs that lexically match queries on junk domains."""
    docs = []
    counter = 0
    for q in QUERIES:
        for style in range(3):
            counter += 1
            dom = list(ABSENT_DOMAINS)[counter % len(ABSENT_DOMAINS)]
            qw = q["text"]
            titles = [
                f"Top 10 {qw.title()} — you won't believe #7!",
                f"AMAZING {qw.upper()} guide — secrets exposed",
                f"{qw.title()} solution that actually works (2026)",
                f"#1 rated {qw} — click now for the fix",
            ]
            snips = [
                f"We tested {qw} and found the ultimate trick. Read before you buy. FREE checklist inside.",
                f"Discover {qw} the pros don't want you to know. Limited-time secrets, guaranteed results.",
                f"Your #1 source for {qw}. Best tips, reviews, and deals. Visit us for the full story.",
            ]
            docs.append({
                "id": f"spam{counter:03d}",
                "url": f"https://{dom}/blog/{qw.replace(' ', '-')}-{counter}",
                "domain": dom,
                "title": RNG.choice(titles),
                "snippet": RNG.choice(snips),
                "published": iso_days_ago(RNG.uniform(1, 15)),
            })
    return docs


def _relevant_docs():
    docs = []
    for q in QUERIES:
        for domain, path, title, snippet, days in q["relevant"]:
            url = f"https://{domain}/{path}"
            docs.append({
                "id": "",
                "url": url,
                "domain": base_domain(url),
                "title": title,
                "snippet": snippet,
                "published": iso_days_ago(days),
                "_relq": q["id"],
            })
    return docs


def build():
    os.makedirs(DATA_DIR, exist_ok=True)
    docs = _relevant_docs() + _spam_docs() + _filler_docs()
    # stable ids
    for i, d in enumerate(docs):
        d["id"] = f"d{i:05d}"

    q_meta = []
    for q in QUERIES:
        ids = [d["id"] for d in docs if d.get("_relq") == q["id"]]
        q_meta.append({"id": q["id"], "category": q["category"], "text": q["text"], "relevant": ids})

    for d in docs:
        d.pop("_relq", None)

    with open(os.path.join(DATA_DIR, "corpus.jsonl"), "w", encoding="utf-8") as f:
        for d in docs:
            f.write(json.dumps(d, ensure_ascii=False) + "\n")
    with open(os.path.join(DATA_DIR, "queries.json"), "w", encoding="utf-8") as f:
        json.dump(q_meta, f, ensure_ascii=False, indent=2)

    # diagnostics: authority presence among relevant docs
    doms = set(load_top100k())
    relevant_total = sum(len(q["relevant"]) for q in q_meta)
    spam_total = len([d for d in docs if d["domain"] in ABSENT_DOMAINS and not d.get("_was_rel")])
    print(f"total docs: {len(docs)}  (relevant={relevant_total}, filler={len([d for d in docs if d['id'].startswith('fill')])})")
    missing = 0
    for q in q_meta:
        bad = [d["id"] for d in docs if d["id"] in q["relevant"] and base_domain(d["url"]) not in doms]
        missing += len(bad)
        print(f"  {q['id']} {q['text'][:28]:30s} relevant={len(q['relevant'])} absent-prior={bad}")
    print("total relevant docs on absent-prior domains:", missing)
    return docs, q_meta


if __name__ == "__main__":
    build()
