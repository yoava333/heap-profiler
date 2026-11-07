"""Create a small heap profile for exploring the Go pprof web interface."""
import argparse
from pathlib import Path

import heap_profiler


def decode_document():
    return bytearray(1024 * 1024)


def load_documents(count):
    documents = []
    for _ in range(count):
        documents.append(decode_document())
    return documents


def build_embeddings(count):
    embeddings = []
    for _ in range(count):
        embeddings.append(bytearray(512 * 1024))
    return embeddings


def build_postings(count):
    postings = []
    for _ in range(count):
        postings.append(bytearray(256 * 1024))
    return postings


def build_search_index(documents):
    return {
        "embeddings": build_embeddings(len(documents)),
        "postings": build_postings(len(documents)),
    }


def render_response():
    return bytearray(512 * 1024)


def cache_responses(count):
    responses = []
    for _ in range(count):
        responses.append(render_response())
    return responses


def process_temporary_batch():
    scratch = []
    for _ in range(4):
        scratch.append(bytearray(2 * 1024 * 1024))
    return sum(map(len, scratch))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=Path("heap.pb.gz"))
    parser.add_argument("--sample-rate", type=int, default=128 * 1024)
    args = parser.parse_args()

    heap_profiler.start_tracing(args.sample_rate, native=False)
    try:
        documents = load_documents(24)
        index = build_search_index(documents)
        responses = cache_responses(12)
        temporary_bytes = process_temporary_batch()
        # Capture while the documents, index, and cache are still live.
        profile = heap_profiler.get_pprof_data(compress=True)
    finally:
        heap_profiler.stop_tracing()

    args.output.write_bytes(profile)
    retained_bytes = (
        sum(map(len, documents))
        + sum(map(len, index["embeddings"]))
        + sum(map(len, index["postings"]))
        + sum(map(len, responses))
    )
    print(f"Profile written to {args.output}")
    print(f"Retained payload: {retained_bytes / 1024**2:.0f} MiB; temporary payload: {temporary_bytes / 1024**2:.0f} MiB")


if __name__ == "__main__":
    main()
