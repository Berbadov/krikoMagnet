from sites.planner import classify, plan_sites
from sites.registry import load_registry, load_tranco_ranks


def setup_module(module):
    module.REG = load_registry()
    module.RANKS = load_tranco_ranks()


def test_turkish_language_and_topics():
    plan = classify("iphone 15 ekran çizgi")
    assert plan["language"] == "tr"
    assert "phones" in plan["topics"]
    assert "apple" in plan["topics"]


def test_turkish_query_prefers_turkish_sites():
    plan = plan_sites("iphone 15 ekran çizgi", REG, RANKS, max_sites=6)
    assert plan["sites"][0] in {"donanimhaber", "webtekno", "chip"}
    assert any(s in plan["sites"] for s in ("donanimhaber", "webtekno", "chip"))


def test_recall_query_prefers_cpsc():
    plan = plan_sites("anker a1257 recall", REG, RANKS, max_sites=6)
    assert plan["sites"][0] == "cpsc"


def test_car_query_prefers_caranddriver():
    plan = plan_sites("honda cr-v review", REG, RANKS, max_sites=6)
    assert plan["sites"][0] == "caranddriver"


def test_boiler_query_prefers_vaillant():
    plan = plan_sites("vaillant ecotec fault code f28", REG, RANKS, max_sites=6)
    assert plan["sites"][0] == "vaillant"


def test_tv_query_prefers_rtings_or_whathifi():
    plan = plan_sites("lg c1 burn in", REG, RANKS, max_sites=6)
    assert plan["sites"][0] in {"rtings", "whathifi"}


def test_skipped_sites_are_documented():
    skipped = {s["domain"] for s in REG["skipped"]}
    assert "sikayetvar.com" in skipped
    assert "old.reddit.com" in skipped
    assert "trendyol.com" in skipped


def test_turkish_fault_query_prefers_turkish_vaillant():
    plan = plan_sites("vaillant kombi arıza kodu", REG, RANKS, max_sites=6)
    assert plan["sites"][0] == "vaillanttr"


def test_generic_dishwasher_query_includes_bosch_home():
    plan = plan_sites("bulaşık makinesi su almıyor", REG, RANKS, max_sites=6)
    assert "boschhome" in plan["sites"]
