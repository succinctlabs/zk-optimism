use std::fs;

use anyhow::Result;
use cargo_metadata::MetadataCommand;
use clap::Parser;
use client_utils::{RawBootInfo, BOOT_INFO_SIZE};
use host_utils::{
    fetcher::{ChainMode, SP1KonaDataFetcher},
    get_agg_proof_stdin,
};
use sp1_sdk::{utils, HashableKey, ProverClient, SP1Proof, SP1ProofWithPublicValues};
use zkvm_host::utils::fetch_header_preimages;

pub const AGG_ELF: &[u8] = include_bytes!("../../elf/aggregation-client-elf");
pub const MULTI_BLOCK_ELF: &[u8] = include_bytes!("../../elf/validity-client-elf");

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Start L2 block number.
    #[arg(short, long, num_args = 1.., value_delimiter = ',')]
    proofs: Vec<String>,

    /// The block number corresponding to the latest L1 checkpoint.
    #[arg(short, long)]
    latest_checkpoint_head_nb: u64,

    /// Prove flag.
    #[arg(short, long)]
    prove: bool,
}

/// Load the aggregation proof data.
fn load_aggregation_proof_data(
    proof_names: Vec<String>,
    l2_chain_id: u64,
) -> (Vec<SP1Proof>, Vec<RawBootInfo>) {
    let metadata = MetadataCommand::new().exec().unwrap();
    let workspace_root = metadata.workspace_root;
    let proof_directory = format!("{}/data/{}/proofs", workspace_root, l2_chain_id);

    let mut proofs = Vec::with_capacity(proof_names.len());
    let mut boot_infos = Vec::with_capacity(proof_names.len());

    for proof_name in proof_names.iter() {
        let proof_path = format!("{}/{}.bin", proof_directory, proof_name);
        if fs::metadata(&proof_path).is_err() {
            panic!("Proof file not found: {}", proof_path);
        }
        let mut deserialized_proof =
            SP1ProofWithPublicValues::load(proof_path).expect("loading proof failed");
        proofs.push(deserialized_proof.proof);

        // The public values are the ABI-encoded BootInfo.
        let mut boot_info_buf = [0u8; BOOT_INFO_SIZE];
        deserialized_proof
            .public_values
            .read_slice(&mut boot_info_buf);
        let boot_info = RawBootInfo::abi_decode(&boot_info_buf).unwrap();
        boot_infos.push(boot_info);
    }

    (proofs, boot_infos)
}

// Execute the Kona program for a single block.
#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();
    utils::setup_logger();

    let args = Args::parse();
    let prover = ProverClient::new();
    let fetcher = SP1KonaDataFetcher::new();

    let l2_chain_id = fetcher.get_chain_id(ChainMode::L2).await?;
    let (proofs, boot_infos) = load_aggregation_proof_data(args.proofs, l2_chain_id);
    let latest_checkpoint_head = fetcher
        .get_header_by_number(ChainMode::L1, args.latest_checkpoint_head_nb)
        .await?
        .hash_slow();
    let headers = fetch_header_preimages(&boot_infos, latest_checkpoint_head).await?;

    let (_, vkey) = prover.setup(MULTI_BLOCK_ELF);

    println!(
        "Multi-block ELF Verification Key U32 Hash: {:?}",
        vkey.vk.hash_u32()
    );

    let stdin =
        get_agg_proof_stdin(proofs, boot_infos, headers, &vkey, latest_checkpoint_head).unwrap();

    let (agg_pk, agg_vk) = prover.setup(AGG_ELF);
    println!("Aggregate ELF Verification Key: {:?}", agg_vk.vk.bytes32());

    if args.prove {
        prover
            .prove(&agg_pk, stdin)
            .plonk()
            .run()
            .expect("proving failed");
    } else {
        let (_, report) = prover.execute(AGG_ELF, stdin).run().unwrap();
        println!("report: {:?}", report);
    }

    Ok(())
}
