mod common;

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use common::{EpubBuilder, EpubVersion, private_tempdir};
use ebook_hero::Error;
use ebook_hero::embed::Models;
use ebook_hero::index::book::Book;
use ebook_hero::index::cache::{EmbeddingStamp, Manifest};
use ebook_hero::index::vectors::Vectors;
use ebook_hero::search::semantic_hits;
use ebook_hero::session::Session;
use ebook_hero::settings::{EMBED_BATCH, EMBEDDING_MODEL, SEMANTIC_FLOOR};
use tempfile::TempDir;

const DIM: usize = 384;

/// An EPUB with one chapter per entry of `paragraphs`, each under its own
/// heading, so that entry `i` is passage `i`.
struct Fixture {
    root: TempDir,
    _books: TempDir,
    session: Session,
    path: String,
}

impl Fixture {
    /// A fixture whose session looks for models in `models_dir`.
    fn new(paragraphs: &[&str], models_dir: PathBuf) -> Fixture {
        let root = private_tempdir();
        let books = TempDir::new().unwrap();
        let path = books.path().join("book.epub");
        let mut builder = EpubBuilder::new(EpubVersion::V3).title("Semantic Book");
        for (i, text) in paragraphs.iter().enumerate() {
            let href = format!("ch{i}.xhtml");
            builder = builder
                .chapter(&href, &format!("<h1>Part {i}</h1><p>{text}</p>"))
                .toc(&format!("Part {i}"), &href, 1);
        }
        builder.write(&path);
        Fixture {
            session: Session::with_dirs(root.path().to_path_buf(), models_dir),
            path: path.to_str().unwrap().to_string(),
            root,
            _books: books,
        }
    }

    fn book(&self) -> Arc<Book> {
        self.session.book(&self.path).unwrap()
    }
}

/// A models directory that is a regular file, so that no model can be loaded.
fn unusable_models() -> (tempfile::NamedTempFile, PathBuf) {
    let file = tempfile::NamedTempFile::new().unwrap();
    let path = file.path().to_path_buf();
    (file, path)
}

fn write_vectors(path: &Path, rows: usize, dim: usize) {
    let rows = (0..rows)
        .map(|i| {
            let mut row = vec![0.0; dim];
            row[i % dim] = 1.0;
            row
        })
        .collect();
    Vectors::from_rows(rows).unwrap().write(path).unwrap();
}

fn read_manifest(book: &Book) -> Manifest {
    serde_json::from_slice(&fs::read(book.dir.join("manifest.json")).unwrap()).unwrap()
}

fn write_manifest(book: &Book, embedding: Option<EmbeddingStamp>) {
    let manifest = Manifest {
        embedding,
        ..read_manifest(book)
    };
    fs::write(
        book.dir.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
}

fn stamp(model: &str, dim: usize) -> Option<EmbeddingStamp> {
    Some(EmbeddingStamp {
        model: model.to_string(),
        dim,
    })
}

const THREE: [&str; 3] = [
    "The first paragraph is about harbours.",
    "The second paragraph is about tides.",
    "The third paragraph is about bread.",
];

#[test]
fn vectors_are_ready_only_with_a_matching_stamp_and_file() {
    let (_file, unusable) = unusable_models();
    let fixture = Fixture::new(&THREE, unusable);
    let book = fixture.book();
    let file = book.dir.join("vectors.f32");
    let n = book.passages.len();
    assert_eq!(n, 3);
    assert!(!book.vectors_ready(), "nothing was embedded yet");

    write_vectors(&file, n, DIM);
    assert!(!book.vectors_ready(), "a file without a stamp");

    write_manifest(&book, stamp(EMBEDDING_MODEL, DIM));
    assert!(book.vectors_ready());

    write_manifest(&book, stamp("another/model", DIM));
    assert!(!book.vectors_ready(), "another model made them");
    write_manifest(&book, stamp(EMBEDDING_MODEL, DIM + 1));
    assert!(!book.vectors_ready(), "another dimension");

    write_manifest(&book, stamp(EMBEDDING_MODEL, DIM));
    write_vectors(&file, n - 1, DIM);
    assert!(!book.vectors_ready(), "one passage has no vector");
    fs::remove_file(&file).unwrap();
    assert!(!book.vectors_ready(), "no file");
}

#[test]
fn stored_vectors_load_without_a_model() {
    let (_file, unusable) = unusable_models();
    let fixture = Fixture::new(&THREE, unusable);
    let book = fixture.book();
    write_vectors(&book.dir.join("vectors.f32"), book.passages.len(), DIM);
    write_manifest(&book, stamp(EMBEDDING_MODEL, DIM));

    let embedded = RefCell::new(0);
    let progress = |_: usize, _: usize| *embedded.borrow_mut() += 1;
    let vectors = book.vectors(fixture.session.models(), &progress).unwrap();
    assert_eq!((vectors.rows(), vectors.dim()), (3, DIM));
    assert_eq!(*embedded.borrow(), 0, "nothing needed embedding");

    // Once loaded, the vectors stay ready even if the file goes away.
    fs::remove_file(book.dir.join("vectors.f32")).unwrap();
    assert!(book.vectors_ready());
    assert!(book.vectors(fixture.session.models(), &|_, _| {}).is_ok());
}

#[test]
fn a_file_of_the_wrong_size_is_embedded_again_and_left_alone_if_that_fails() {
    let (_file, unusable) = unusable_models();
    let fixture = Fixture::new(&THREE, unusable);
    let book = fixture.book();
    let file = book.dir.join("vectors.f32");
    write_vectors(&file, book.passages.len() - 1, DIM);
    write_manifest(&book, stamp(EMBEDDING_MODEL, DIM));
    let before = fs::read(&file).unwrap();

    let error = book
        .vectors(fixture.session.models(), &|_, _| {})
        .err()
        .unwrap();
    assert!(matches!(error, Error::ModelUnavailable { .. }), "{error}");
    assert_eq!(fs::read(&file).unwrap(), before);
    assert!(!book.dir.join("vectors.f32.tmp").exists());
    assert!(!book.vectors_ready());
    assert_eq!(read_manifest(&book).embedding, stamp(EMBEDDING_MODEL, DIM));
}

#[test]
fn embedding_a_book_whose_cache_was_removed_names_the_remedy() {
    let (_file, unusable) = unusable_models();
    let fixture = Fixture::new(&THREE, unusable);
    let book = fixture.book();
    fs::remove_dir_all(&book.dir).unwrap();

    let error = book
        .vectors(fixture.session.models(), &|_, _| {})
        .err()
        .unwrap();

    assert!(
        matches!(&error, Error::Index(message)
            if message == "the cached index for this book was removed; call again to rebuild it"),
        "{error}"
    );
    assert!(!book.dir.exists());
}

#[test]
fn semantic_search_without_a_model_names_the_remedy() {
    let (_file, unusable) = unusable_models();
    let fixture = Fixture::new(&THREE, unusable);
    let book = fixture.book();
    let error = semantic_hits(&book, fixture.session.models(), "harbour", 5, &|_, _| {})
        .err()
        .unwrap();
    assert!(error.to_string().contains("mode=lexical"), "{error}");
    assert!(!book.dir.join("vectors.f32").exists());
}

#[test]
fn a_session_looks_for_models_in_the_directory_it_was_given() {
    let (_file, unusable) = unusable_models();
    let root = private_tempdir();
    let session = Session::with_dirs(root.path().to_path_buf(), unusable.clone());
    match session.models().embed_query("x").unwrap_err() {
        Error::ModelUnavailable { cache, .. } => assert_eq!(cache, unusable),
        other => panic!("expected ModelUnavailable, got {other:?}"),
    }
}

fn models() -> Models {
    Models::new(Models::default_cache_dir())
}

#[test]
#[ignore = "needs the embedding model"]
fn embeds_384_dimensions() {
    let models = models();
    let texts: Vec<&str> = (0..EMBED_BATCH + 2).map(|_| "A short passage.").collect();
    let calls = RefCell::new(Vec::new());
    let rows = models
        .embed_passages(&texts, &|done, total| {
            calls.borrow_mut().push((done, total))
        })
        .unwrap();
    assert_eq!(rows.len(), texts.len());
    assert!(rows.iter().all(|row| row.len() == DIM));
    let total = texts.len();
    assert_eq!(*calls.borrow(), [(EMBED_BATCH, total), (total, total)]);

    let query = models.embed_query("a short passage").unwrap();
    assert_eq!(query.len(), DIM);
    let norm: f32 = query.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-3, "{norm}");
}

#[test]
#[ignore = "needs the embedding and reranker models"]
fn rerank_scores_follow_the_input_order() {
    let scores = models()
        .rerank(
            "how do tides work?",
            &[
                "The recipe calls for two cups of flour.",
                "Tides rise and fall because of the pull of the moon.",
                "Interest rates moved little.",
            ],
        )
        .unwrap();
    assert_eq!(scores.len(), 3);
    assert!(scores[1] > scores[0] && scores[1] > scores[2], "{scores:?}");
}

#[test]
#[ignore = "needs the embedding model"]
fn stage2_builds_once_and_stamps_manifest() {
    let fixture = Fixture::new(&THREE, Models::default_cache_dir());
    let book = fixture.book();
    let n = book.passages.len();
    assert!(!book.vectors_ready());

    let calls = RefCell::new(Vec::new());
    let progress = |done: usize, total: usize| calls.borrow_mut().push((done, total));
    let vectors = book.vectors(fixture.session.models(), &progress).unwrap();
    assert_eq!((vectors.rows(), vectors.dim()), (n, DIM));
    assert_eq!(calls.borrow().last(), Some(&(n, n)));

    let manifest = read_manifest(&book);
    assert_eq!(manifest.embedding, stamp(EMBEDDING_MODEL, DIM));
    assert!(manifest.is_current_stage1());
    let file = book.dir.join("vectors.f32");
    assert_eq!(fs::metadata(&file).unwrap().len(), (n * DIM * 4) as u64);
    assert!(!book.dir.join("vectors.f32.tmp").exists());
    assert!(book.vectors_ready());

    // The same book again does not embed.
    let before = calls.borrow().len();
    book.vectors(fixture.session.models(), &progress).unwrap();
    assert_eq!(calls.borrow().len(), before);

    // A new session over the same cache loads the file and needs no model.
    let (_file, unusable) = unusable_models();
    let second = Session::with_dirs(fixture.root.path().to_path_buf(), unusable);
    let reopened = second.book(&fixture.path).unwrap();
    assert!(reopened.vectors_ready());
    let loaded = reopened
        .vectors(second.models(), &|_, _| panic!("embedded again"))
        .unwrap();
    assert_eq!((loaded.rows(), loaded.dim()), (n, DIM));
}

#[test]
#[ignore = "needs the embedding model"]
fn concurrent_sessions_embed_a_book_once() {
    let fixture = Fixture::new(&THREE, Models::default_cache_dir());
    fixture.book();
    let embedding_steps = AtomicUsize::new(0);
    let barrier = Barrier::new(2);
    thread::scope(|scope| {
        for _ in 0..2 {
            scope.spawn(|| {
                let session = Session::with_dirs(
                    fixture.root.path().to_path_buf(),
                    Models::default_cache_dir(),
                );
                let book = session.book(&fixture.path).unwrap();
                barrier.wait();
                let vectors = book
                    .vectors(session.models(), &|_, _| {
                        embedding_steps.fetch_add(1, Ordering::SeqCst);
                    })
                    .unwrap();
                assert_eq!(vectors.rows(), THREE.len());
            });
        }
    });
    // Three passages are one batch, and only one session embedded them.
    assert_eq!(embedding_steps.load(Ordering::SeqCst), 1);
}

#[test]
#[ignore = "needs the embedding model"]
fn semantic_hit_finds_paraphrase() {
    let fixture = Fixture::new(
        &[
            "The council voted to widen the harbour road and to levy a new toll on freight carts.",
            "There was a bottle labelled drink me.",
            "Tides rise and fall twice a day because of the gravitational pull of the moon.",
            "The recipe calls for two cups of flour, a pinch of salt and three eggs.",
            "Interest rates on long-term bonds moved little after the central bank met.",
        ],
        Models::default_cache_dir(),
    );
    let book = fixture.book();
    let hits = semantic_hits(
        &book,
        fixture.session.models(),
        "a drink with a label",
        3,
        &|_, _| {},
    )
    .unwrap();

    assert!(!hits.is_empty() && hits.len() <= 3, "{hits:?}");
    let first = &hits[0];
    assert_eq!(first.passage_id, 1, "{hits:?}");
    assert!(first.text.contains("drink me"));
    assert_eq!(first.semantic_rank, Some(1));
    assert_eq!(first.lexical_rank, None);
    assert!(first.score >= SEMANTIC_FLOOR && first.score <= 1.0 + 1e-3);
    assert!(!first.matches.is_empty(), "the query words are highlighted");
    for (i, hit) in hits.iter().enumerate() {
        assert_eq!(hit.semantic_rank, Some(i as u32 + 1));
        assert!(hit.score >= SEMANTIC_FLOOR);
    }
    assert!(hits.windows(2).all(|pair| pair[0].score >= pair[1].score));
}

#[test]
#[ignore = "needs the embedding model"]
fn nothing_below_the_floor_is_returned() {
    let fixture = Fixture::new(&THREE, Models::default_cache_dir());
    let book = fixture.book();
    let hits = semantic_hits(
        &book,
        fixture.session.models(),
        "quantum chromodynamics of neutron stars",
        5,
        &|_, _| {},
    )
    .unwrap();
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn a_limit_of_zero_embeds_nothing_and_needs_no_model() {
    let (_file, unusable) = unusable_models();
    let fixture = Fixture::new(&THREE, unusable);
    let book = fixture.book();
    let embedded = RefCell::new(0);
    let progress = |_: usize, _: usize| *embedded.borrow_mut() += 1;
    let hits = semantic_hits(&book, fixture.session.models(), "harbour", 0, &progress).unwrap();
    assert!(hits.is_empty());
    assert_eq!(*embedded.borrow(), 0);
    assert!(
        !book.dir.join("vectors.f32").exists(),
        "the book was not embedded"
    );
}

#[test]
#[ignore = "needs the embedding model"]
fn a_cancelled_embedding_stops_between_batches_and_writes_nothing() {
    let texts: Vec<String> = (0..EMBED_BATCH * 3)
        .map(|i| format!("Passage number {i} is about subject {i} and nothing else."))
        .collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let fixture = Fixture::new(&refs, Models::default_cache_dir());
    let cancel = ebook_hero::cancel::Cancel::new();
    let cancellable = fixture.session.cancellable(cancel.clone());
    let book = cancellable.book(&fixture.path).unwrap();
    let batches = AtomicUsize::new(0);

    let result = book.vectors_cancellable(
        cancellable.models(),
        &|_, _| {
            batches.fetch_add(1, Ordering::SeqCst);
            cancel.cancel();
        },
        &cancel,
    );

    assert!(
        matches!(result, Err(Error::Cancelled)),
        "{:?}",
        result.err()
    );
    assert_eq!(
        batches.load(Ordering::SeqCst),
        1,
        "it stopped after one batch"
    );
    assert!(!book.vectors_ready(), "a cancelled embedding is not cached");
    assert!(!book.dir.join("vectors.f32").exists());

    // The session it came from embeds the book as if nothing had happened.
    let vectors = book.vectors(fixture.session.models(), &|_, _| {}).unwrap();
    assert_eq!(vectors.rows(), book.passages.len());
}
