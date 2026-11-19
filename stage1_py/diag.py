import json
import sys
from ranker import Ranker

qid = sys.argv[1] if len(sys.argv) > 1 else "q6"
weights = json.loads(sys.argv[2]) if len(sys.argv) > 2 else None

r = Ranker(weights=weights)
r.load_corpus()
queries = r.load_queries()
q = next(x for x in queries if x["id"] == qid)
rel = set(q["relevant"])

out = r.rank(q["text"], top_k=15, return_scores=True)
print(f"QUERY: {q['text']}  (relevant={len(rel)})")
print(f"{'id':<8}{'domain':<28}{'rel':<4}{'score':<7}{'bm25s':<7}{'priors':<7}{'fresh':<6} title")
print("-" * 120)
for doc, s, bm_s, prior_s, fresh in out:
    tag = "R" if doc["id"] in rel else ("S" if doc["domain"] in {
        "techseohub.net","bestgadgetdeals.top","buyguideonline.com","seovault.xyz",
        "theproductsco.com","dealmasterz.com","quicktopten.com","reviewsuite.io",
        "gadgetsmarts.org","top10findings.com","wellnessmagic.xyz","dealseekerz.top",
        "travelhackzz.com","techreviewhub.net","shoppydeals.live","boostpicks.com",
        "rankedreviews.net","cheapboon.com","findscoop.com","thebestlist.org"} else " ")
    print(f"{doc['id']:<8}{doc['domain']:<28}{tag:<4}{s:<7.4f}{bm_s:<7.3f}{prior_s:<7.3f}{fresh:<6.3f} {doc['title'][:50]}")
