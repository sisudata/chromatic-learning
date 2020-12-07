//! Performs greedy coloring over the feature co-occurrence graph
//! incrementally, creating new files with categorical svmlight
//! features encoding the chromatic encoding defined by the coloring.

use std::collections::HashMap;
use std::convert::TryInto;
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;
use std::u32;

use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use serde_json::json;
use structopt::StructOpt;

use crank::{color, simsvm, Scanner, SummaryStats};

/// Reads utf-8 text files in simsvm format. "simplified svmlight" format.
///
/// Creates adjacent files `<original_filename>_greedy_<k>` with
/// the same <target> value on each line, but features mapped to
/// a smaller new space, now with several positive integers
/// representing new categorical variables:
/// <target> <new index>:<feature index> <new index>:<feature index>...
/// the new features are 1-indexed (for 0 to represent an abence of
/// features associated with the new index).
///
/// The option <k> defines the minimum number of times an edge needs to
/// appear in the co-occurrence graph of features (its minimum weight)
/// to be considered part of the graph.
///
/// The transformation is done for both train and test files, but
/// only train files are used to construct the graph.
#[derive(Debug, StructOpt)]
#[structopt(name = "greedy", about = "Recoding a list of files with map.")]
struct Opt {
    /// Training files to use for coloring and transformation.
    #[structopt(long)]
    train: Vec<PathBuf>,

    /// Test files to transform.
    #[structopt(long)]
    test: Vec<PathBuf>,

    /// Minimum edge weight `k` for use in coloring. Must be at least 1.
    #[structopt(long)]
    k: u32,
}

fn main() {
    let opt = Opt::from_args();
    assert!(opt.k > 0);
    let scanner = Scanner::new(opt.train, b' ');
    let train_stats_start = Instant::now();
    let train_stats = simsvm::stats(&scanner);
    println!(
        "{}",
        json!({
            "dataset": "train",
            "nfeatures": train_stats.nfeatures(),
            "npages": train_stats.npages(),
            "nrows": train_stats.nrows(),
            "avg_nnz": train_stats.avg_nnz(),
            "stats_duration": format!("{:.0?}", Instant::now().duration_since(train_stats_start)),
        })
    );

    let csr_start = Instant::now();
    let csr = simsvm::csr(&scanner, &train_stats);
    println!(
        "{}",
        json!({ "csr_duration": format!("{:.0?}", Instant::now().duration_since(csr_start)) })
    );

    let csc_start = Instant::now();
    let csc: Vec<_> = csr.par_iter().map(|mat| mat.clone().into_csc()).collect();
    println!(
        "{}",
        json!({ "csc_duration": format!("{:.0?}", Instant::now().duration_since(csc_start)) })
    );

    let colors_start = Instant::now();
    let (colors, remap) = color::greedy(&csr, &csc, opt.k);
    println!(
        "{}",
        json!({
            "ncolors": compute_ncolors(&colors),
            "color_cardinalities": compute_color_cardinalities(&colors, &remap),
            "colors_duration": format!("{:.0?}", Instant::now().duration_since(colors_start)),
        })
    );

    let encode_start = Instant::now();
    // TODO: collision stats
    encode(&colors, &remap, &scanner, opt.k);

    // TODO: timings for test stats, encode, collision stats
    let scanner = Scanner::new(opt.test, b' ');
    let test_start = Instant::now();
    let test_stats = simsvm::stats(&scanner);
    println!(
        "{}",
        json!({
            "dataset": "test",
            "nfeatures": test_stats.nfeatures(),
            "npages": test_stats.npages(),
            "nrows": test_stats.nrows(),
            "avg_nnz": format!("{:.1}", train_stats.avg_nnz()),
            "stats_duration": format!("{:.0?}", Instant::now().duration_since(test_start)),
        })
    );
    encode(&colors, &remap, &scanner, opt.k);
}

fn compute_ncolors(colors: &[u32]) -> u32 {
    colors
        .iter()
        .copied()
        .max()
        .map(|x| x + 1)
        .unwrap_or(0)
        .try_into()
        .unwrap()
}

/// Returns a set of summary statistics over the cardinality (number of features)
/// mapping to each color column.
fn compute_color_cardinalities(colors: &[u32], remap: &[u32]) -> HashMap<String, f64> {
    let cards = remap.iter().copied().enumerate().fold(
        HashMap::new(),
        |mut acc: HashMap<u32, u32>, (feature, remap_val)| {
            let entry = acc.entry(colors[feature]).or_default();
            *entry = (*entry).max(remap_val);
            acc
        },
    );
    SummaryStats::from(cards.values().map(|x| *x as f64)).to_map()
}

fn encode(colors: &[u32], remap: &[u32], scanner: &Scanner, k: u32) {
    let ncolors = compute_ncolors(colors) as usize;
    scanner.for_each_sink(
        vec![u32::MAX; ncolors],
        |line, writer, new_features| {
            let line = simsvm::parse(line);
            writer.write_all(line.target()).unwrap();
            line.for_each(|feature| {
                if feature as usize >= colors.len() {
                    return;
                }
                let color = colors[feature as usize];
                let remapped = remap[feature as usize];
                new_features[color as usize] = new_features[color as usize].min(remapped);
            });
            for (i, v) in new_features.iter_mut().enumerate() {
                if *v == u32::MAX {
                    continue;
                }
                write!(writer, " {}:{}", i, *v).unwrap();
                *v = u32::MAX;
            }
            writer.write_all(b"\n").unwrap();
        },
        &format!("_greedy_{}", k),
    );
}