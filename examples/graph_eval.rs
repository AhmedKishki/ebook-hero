//! SPIKE: runs every search variant of `ebook_hero::search::graph_spike` on a
//! book and a query set, and writes what each returned to OUT_DIR/run.json and
//! the book's passages to OUT_DIR/passages.json.
//!
//!     cargo run --release --example graph_eval -- BOOK QUERIES.json OUT_DIR

use std::fs;
use std::path::Path;

use ebook_hero::search::graph_spike::{Concepts, Variant, run};
use ebook_hero::session::Session;
use serde_json::{Map, Value, json};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [book_path, queries_path, out_dir] = args.as_slice() else {
        return Err("usage: graph_eval BOOK QUERIES.json OUT_DIR".into());
    };
    let queries: Vec<Value> = serde_json::from_str(&fs::read_to_string(queries_path)?)?;
    let session = Session::new();
    let book = session.book(book_path)?;
    let graph = book.graph()?;
    let concepts = Concepts::new(graph);
    let models = session.models();
    // `$SPIKE_VARIANTS`, a comma-separated list of names, runs only those.
    let chosen: Vec<Variant> = match std::env::var("SPIKE_VARIANTS") {
        Ok(names) => Variant::ALL
            .into_iter()
            .filter(|v| names.split(',').any(|n| n == v.name()))
            .collect(),
        Err(_) => Variant::ALL.to_vec(),
    };

    eprintln!("warming up: the first call embeds the book and loads both models");
    for &variant in &chosen {
        run(&book, models, &concepts, "warm up the models", variant)?;
    }

    let labels =
        |cs: &[usize]| -> Vec<String> { cs.iter().map(|&c| concepts.labels[c].clone()).collect() };
    let mut rows = Vec::new();
    for (i, query) in queries.iter().enumerate() {
        let text = query["q"].as_str().ok_or("a query has no \"q\"")?;
        let seeds = concepts.query_seeds(text);
        let mut results = Map::new();
        for &variant in &chosen {
            let outcome = run(&book, models, &concepts, text, variant)?;
            results.insert(
                variant.name().to_string(),
                json!({"top": outcome.top, "candidates": outcome.candidates, "ms": outcome.millis}),
            );
        }
        let top5: Vec<u32> = results
            .get("hybrid")
            .and_then(|r| r["top"].as_array())
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_u64)
                    .map(|id| id as u32)
                    .take(5)
                    .collect()
            })
            .unwrap_or_default();
        let annotated: Vec<Vec<String>> = top5
            .iter()
            .map(|&id| labels(concepts.of_passage(id)))
            .collect();
        rows.push(json!({
            "id": query["id"],
            "type": query["type"],
            "q": text,
            "seeds": labels(&seeds.iter().map(|s| s.0).collect::<Vec<_>>()),
            "expansions": concepts.expansions(&seeds).into_iter().map(|e| e.0).collect::<Vec<_>>(),
            "hybrid_top5_concepts": annotated,
            "results": results,
        }));
        eprintln!("{}/{} {}", i + 1, queries.len(), text);
    }

    let out = Path::new(out_dir);
    fs::create_dir_all(out)?;
    let summary = json!({
        "book": {
            "path": book_path,
            "passages": book.passages.len(),
            "concepts": graph.concepts.len(),
            "links": graph.links.len(),
            "communities": graph.communities.len(),
            "coverage": concepts.coverage(),
            "labels": concepts.labels,
        },
        "queries": rows,
    });
    fs::write(
        out.join("run.json"),
        serde_json::to_string_pretty(&summary)?,
    )?;
    let passages: Vec<Value> = book
        .passages
        .iter()
        .map(|p| json!({"id": p.id, "section": p.section.join(" › "), "text": p.text}))
        .collect();
    fs::write(out.join("passages.json"), serde_json::to_string(&passages)?)?;
    Ok(())
}
