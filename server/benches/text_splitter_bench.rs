//! Benchmarks for text splitting and punctuation detection.
//!
//! Tests multi-language sentence segmentation, overlap merging, and deduplication.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use realtime::asr::punctuation::{find_last_sentence_terminal, find_last_weak_break, is_only_punctuation, is_sentence_terminal, is_weak_break_point};
use realtime::rpc::pipeline::asr_llm_tts::asr_task_base::{remove_consecutive_duplicates, smart_text_merge};
use realtime::text_splitter::SimplifiedStreamingSplitter;

fn bench_punctuation_detection(c: &mut Criterion) {
    let mut group = c.benchmark_group("punctuation");

    group.bench_function("is_sentence_terminal/period", |b| {
        b.iter(|| black_box(is_sentence_terminal('.')));
    });
    group.bench_function("is_sentence_terminal/chinese", |b| {
        b.iter(|| black_box(is_sentence_terminal('。')));
    });
    group.bench_function("is_sentence_terminal/letter", |b| {
        b.iter(|| black_box(is_sentence_terminal('a')));
    });
    group.bench_function("is_weak_break_point/comma", |b| {
        b.iter(|| black_box(is_weak_break_point(',')));
    });
    group.bench_function("is_only_punctuation/true", |b| {
        b.iter(|| black_box(is_only_punctuation("。！？...")));
    });
    group.bench_function("is_only_punctuation/false", |b| {
        b.iter(|| black_box(is_only_punctuation("Hello, world!")));
    });

    let long_text = "这是一段很长的中文文本，包含了多个句子。第二句话在这里！第三句话呢？最后一句。";
    group.bench_function("find_last_sentence_terminal/chinese_80char", |b| {
        b.iter(|| black_box(find_last_sentence_terminal(long_text)));
    });

    let english_text = "This is a long English sentence with multiple clauses, and it keeps going. Here is another sentence! And a question?";
    group.bench_function("find_last_sentence_terminal/english_120char", |b| {
        b.iter(|| black_box(find_last_sentence_terminal(english_text)));
    });

    group.bench_function("find_last_weak_break/chinese", |b| {
        b.iter(|| black_box(find_last_weak_break("第一部分，第二部分，第三部分")));
    });

    group.finish();
}

fn bench_smart_text_merge(c: &mut Criterion) {
    let mut group = c.benchmark_group("smart_text_merge");

    group.bench_function("no_overlap", |b| {
        b.iter(|| black_box(smart_text_merge("Hello", "World")));
    });
    group.bench_function("full_containment", |b| {
        b.iter(|| black_box(smart_text_merge("Hello", "Hello World")));
    });
    group.bench_function("partial_overlap", |b| {
        b.iter(|| black_box(smart_text_merge("Hello Wor", "World")));
    });
    group.bench_function("chinese_no_overlap", |b| {
        b.iter(|| black_box(smart_text_merge("你好世界", "欢迎来到")));
    });
    group.bench_function("chinese_overlap", |b| {
        b.iter(|| black_box(smart_text_merge("你好世界欢", "欢迎来到")));
    });

    group.finish();
}

fn bench_remove_consecutive_duplicates(c: &mut Criterion) {
    let mut group = c.benchmark_group("remove_consecutive_duplicates");

    group.bench_function("no_dups", |b| {
        b.iter(|| black_box(remove_consecutive_duplicates("one two three four five")));
    });
    group.bench_function("word_dups", |b| {
        b.iter(|| black_box(remove_consecutive_duplicates("hello hello world world foo")));
    });
    group.bench_function("phrase_dups", |b| {
        b.iter(|| black_box(remove_consecutive_duplicates("hello world hello world this is fine")));
    });
    group.bench_function("chinese_dups", |b| {
        b.iter(|| black_box(remove_consecutive_duplicates("你好你好世界世界")));
    });

    group.finish();
}

fn bench_streaming_splitter(c: &mut Criterion) {
    let mut group = c.benchmark_group("streaming_splitter");

    // Chinese streaming input (typical LLM output)
    let chinese_chunks = [
        "今天", "天气", "真不", "错，", "我们", "可以", "去公", "园散", "步。", "你觉", "得怎", "么样", "？",
    ];

    group.bench_function("chinese_streaming/13_chunks", |b| {
        b.iter(|| {
            let mut splitter = SimplifiedStreamingSplitter::new(None);
            let mut results = Vec::new();
            for chunk in &chinese_chunks {
                results.extend(splitter.found_first_sentence(chunk));
            }
            results.extend(splitter.finalize());
            black_box(results);
        });
    });

    // English streaming input
    let english_chunks = [
        "The weather ",
        "is really ",
        "nice today. ",
        "We could go ",
        "for a walk ",
        "in the park. ",
        "What do you ",
        "think?",
    ];

    group.bench_function("english_streaming/8_chunks", |b| {
        b.iter(|| {
            let mut splitter = SimplifiedStreamingSplitter::new(None);
            let mut results = Vec::new();
            for chunk in &english_chunks {
                results.extend(splitter.found_first_sentence(chunk));
            }
            results.extend(splitter.finalize());
            black_box(results);
        });
    });

    // Single large input
    let large_input = "这是第一句话。这是第二句话！这是第三句话？这是第四句话，\
                        还有一些补充内容。最后一句话到此结束。";

    group.bench_function("chinese_bulk/single_input", |b| {
        b.iter(|| {
            let mut splitter = SimplifiedStreamingSplitter::new(None);
            let mut results = splitter.found_first_sentence(large_input);
            results.extend(splitter.finalize());
            black_box(results);
        });
    });

    // Mixed language
    let mixed_chunks = ["Hello, ", "你好！", "Today we'll ", "学习中文。", "Isn't that ", "great？"];

    group.bench_function("mixed_language/6_chunks", |b| {
        b.iter(|| {
            let mut splitter = SimplifiedStreamingSplitter::new(None);
            let mut results = Vec::new();
            for chunk in &mixed_chunks {
                results.extend(splitter.found_first_sentence(chunk));
            }
            results.extend(splitter.finalize());
            black_box(results);
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_punctuation_detection,
    bench_smart_text_merge,
    bench_remove_consecutive_duplicates,
    bench_streaming_splitter,
);
criterion_main!(benches);
