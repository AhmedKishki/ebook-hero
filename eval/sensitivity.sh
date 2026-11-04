#!/bin/sh
# SPIKE: reruns the graph variants without the reranker at lower RRF weights
# for the graph list. Usage: eval/sensitivity.sh VEBLEN_EPUB RUNS_DIR
set -eu
veblen=$1
runs=$2
variants=hybrid-norerank,hybrid+ppr-norerank,hybrid+pprfb-norerank,lexical,lexical+ppr
for w in 0.25 0.5; do
    SPIKE_GRAPH_WEIGHT=$w SPIKE_VARIANTS=$variants ./target/release/examples/graph_eval \
        tests/fixtures/alice.epub eval/queries/alice.json "$runs/alice-w$w" 2>&1 | tail -1
    SPIKE_GRAPH_WEIGHT=$w SPIKE_VARIANTS=$variants ./target/release/examples/graph_eval \
        "$veblen" eval/queries/veblen.json "$runs/veblen-w$w" 2>&1 | tail -1
done
