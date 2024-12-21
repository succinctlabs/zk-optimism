use anyhow::Result;
use clap::Parser;
use op_succinct_host_utils::{
    block_range::get_validated_block_range,
    fetcher::{CacheMode, OPSuccinctDataFetcher, RunContext},
    get_proof_stdin,
    stats::ExecutionStats,
    ProgramType,
};
use op_succinct_prove::{execute_multi, generate_witness, DEFAULT_RANGE, RANGE_ELF};
use op_succinct_scripts::HostExecutorArgs;
use sp1_sdk::{
    network_v2::proto::network::{FulfillmentStrategy, ProofMode},
    utils, NetworkProverV2, Prover,
};
use std::{fs, path::PathBuf, time::Duration};

/// Execute the OP Succinct program for multiple blocks.
#[tokio::main]
async fn main() -> Result<()> {
    let args = HostExecutorArgs::parse();

    dotenv::from_path(&args.env_file)?;
    utils::setup_logger();

    let data_fetcher = OPSuccinctDataFetcher::new_with_rollup_config(RunContext::Dev).await?;

    let cache_mode = if args.use_cache {
        CacheMode::KeepCache
    } else {
        CacheMode::DeleteCache
    };

    // If the end block is provided, check that it is less than the latest finalized block. If the end block is not provided, use the latest finalized block.
    let (l2_start_block, l2_end_block) =
        get_validated_block_range(&data_fetcher, args.start, args.end, DEFAULT_RANGE).await?;

    let host_cli = data_fetcher
        .get_host_cli_args(l2_start_block, l2_end_block, ProgramType::Multi, cache_mode)
        .await?;

    // By default, re-run the native execution unless the user passes `--use-cache`.
    let witness_generation_time_sec = if !args.use_cache {
        generate_witness(&host_cli).await?
    } else {
        Duration::ZERO
    };

    // Get the stdin for the block.
    let sp1_stdin = get_proof_stdin(&host_cli)?;

    let private_key = std::env::var("SP1_PRIVATE_KEY")?;
    let rpc_url = std::env::var("PROVER_NETWORK_RPC")?;
    let mut prover = NetworkProverV2::new(&private_key, Some(rpc_url), false);
    prover.with_strategy(FulfillmentStrategy::Reserved);

    if args.prove {
        // If the prove flag is set, generate a proof.
        let (pk, _) = prover.setup(RANGE_ELF);

        // Generate proofs in compressed mode for aggregation verification.
        let proof = prover
            .prove(&pk, sp1_stdin, ProofMode::Compressed, Default::default())
            .await?;

        // Create a proof directory for the chain ID if it doesn't exist.
        let proof_dir = format!(
            "data/{}/proofs",
            data_fetcher.get_l2_chain_id().await.unwrap()
        );
        if !std::path::Path::new(&proof_dir).exists() {
            fs::create_dir_all(&proof_dir).unwrap();
        }
        // Save the proof to the proof directory corresponding to the chain ID.
        proof
            .save(format!(
                "{}/{}-{}.bin",
                proof_dir, l2_start_block, l2_end_block
            ))
            .expect("saving proof failed");
    } else {
        let l2_chain_id = data_fetcher.get_l2_chain_id().await?;

        let (block_data, report, execution_duration) =
            execute_multi(&data_fetcher, sp1_stdin, l2_start_block, l2_end_block).await?;

        let stats = ExecutionStats::new(
            &block_data,
            &report,
            witness_generation_time_sec.as_secs(),
            execution_duration.as_secs(),
        );

        println!("Execution Stats: \n{:?}", stats);

        // Create the report directory if it doesn't exist.
        let report_dir = format!("execution-reports/multi/{}", l2_chain_id);
        if !std::path::Path::new(&report_dir).exists() {
            fs::create_dir_all(&report_dir)?;
        }

        let report_path = format!(
            "execution-reports/multi/{}/{}-{}.csv",
            l2_chain_id, l2_start_block, l2_end_block
        );

        // Write to CSV.
        let mut csv_writer = csv::Writer::from_path(report_path)?;
        csv_writer.serialize(&stats)?;
        csv_writer.flush()?;
    }

    Ok(())
}
