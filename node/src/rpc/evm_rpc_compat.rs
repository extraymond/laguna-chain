use ethereum_types::{H64, U64};
use fc_rpc::format;
use fc_rpc_core::{
	types::{
		BlockNumber, Bytes, CallRequest, FeeHistory, Index, PeerCount, Receipt, RichBlock,
		SyncStatus, Transaction, TransactionRequest, Work,
	},
	EthApiServer, NetApiServer,
};

use codec::Encode;
use fc_rpc_core::types::SyncInfo;
use fp_rpc::ConvertTransactionRuntimeApi;
use jsonrpsee::core::{async_trait, RpcResult as Result};
use pallet_evm_compat_rpc::EvmCompatApiRuntimeApi as EvmCompatRuntimeApi;
use primitives::{AccountId, Balance};
use sc_client_api::{HeaderBackend, StateBackend, StorageProvider};
use sc_network::{ExHashT, NetworkService};
use sc_service::InPoolTransaction;
use sc_transaction_pool::{ChainApi, Pool};
use sc_transaction_pool_api::{TransactionPool, TransactionSource};
use sp_api::{ApiExt, ProvideRuntimeApi};
use sp_block_builder::BlockBuilder as BlockBuilderApi;
use sp_core::{H160, H256, U256};
use sp_runtime::{
	generic::BlockId,
	traits::{BlakeTwo256, Block as BlockT, UniqueSaturatedInto},
};

pub mod block_mapper;
pub mod pending_api;
use block_mapper::BlockMapper;
use pending_api::pending_runtime_api;

use sc_client_api::Backend;
use std::{marker::PhantomData, sync::Arc};

pub struct Net<B: BlockT, C, H: ExHashT> {
	client: Arc<C>,
	network: Arc<NetworkService<B, H>>,
	peer_count_as_hex: bool,
}

impl<B: BlockT, C, H: ExHashT> Net<B, C, H> {
	pub fn new(
		client: Arc<C>,
		network: Arc<NetworkService<B, H>>,
		peer_count_as_hex: bool,
	) -> Self {
		Self { client, network, peer_count_as_hex }
	}
}

pub fn err<T: ToString>(code: i32, message: T, data: Option<&[u8]>) -> jsonrpsee::core::Error {
	jsonrpsee::core::Error::Call(jsonrpsee::types::error::CallError::Custom(
		jsonrpsee::types::error::ErrorObject::owned(
			code,
			message.to_string(),
			data.map(|bytes| {
				jsonrpsee::core::to_json_raw_value(&format!("0x{}", hex::encode(bytes)))
					.expect("fail to serialize data")
			}),
		),
	))
}

pub fn internal_err<T: ToString>(message: T) -> jsonrpsee::core::Error {
	err(jsonrpsee::types::error::INTERNAL_ERROR_CODE, message, None)
}

impl<B, C, H: ExHashT> NetApiServer for Net<B, C, H>
where
	B: BlockT<Hash = H256> + Send + Sync + 'static,
	C: HeaderBackend<B> + ProvideRuntimeApi<B> + Send + Sync + 'static,
	C::Api: EvmCompatRuntimeApi<B, AccountId, Balance>,
{
	fn version(&self) -> Result<String> {
		let hash = self.client.info().best_hash;
		Ok(self
			.client
			.runtime_api()
			.chain_id(&BlockId::Hash(hash))
			.map_err(|_| internal_err("fetch runtime chain id failed"))?
			.to_string())
	}

	fn peer_count(&self) -> Result<PeerCount> {
		let peer_count = self.network.num_connected();
		Ok(match self.peer_count_as_hex {
			true => PeerCount::String(format!("0x{:x}", peer_count)),
			false => PeerCount::U32(peer_count as u32),
		})
	}

	fn is_listening(&self) -> Result<bool> {
		Ok(true)
	}
}

pub struct EthApi<B: BlockT, C, H: ExHashT, CT, BE, P, A: ChainApi> {
	client: Arc<C>,
	network: Arc<NetworkService<B, H>>,
	convert_transaction: Option<CT>,
	pool: Arc<P>,
	graph: Arc<Pool<A>>,
	is_authority: bool,
	_marker: PhantomData<BE>,
}

impl<B: BlockT, C, H: ExHashT, CT, BE, P, A: ChainApi> EthApi<B, C, H, CT, BE, P, A> {
	pub fn new(
		client: Arc<C>,
		pool: Arc<P>,
		graph: Arc<Pool<A>>,
		network: Arc<NetworkService<B, H>>,
		is_authority: bool,
		convert_transaction: Option<CT>,
	) -> Self {
		Self {
			client,
			pool,
			network,
			graph,
			convert_transaction,
			is_authority,
			_marker: Default::default(),
		}
	}
}

#[async_trait]
impl<B, C, H: ExHashT, CT, BE, P, A> EthApiServer for EthApi<B, C, H, CT, BE, P, A>
where
	B: BlockT<Hash = H256> + Send + Sync + 'static,
	C: ProvideRuntimeApi<B> + StorageProvider<B, BE>,
	BE: Backend<B> + 'static,
	BE::State: StateBackend<BlakeTwo256>,
	C::Api: ConvertTransactionRuntimeApi<B>,
	C::Api: EvmCompatRuntimeApi<B, AccountId, Balance>,
	C::Api: BlockBuilderApi<B>,
	C: HeaderBackend<B> + ProvideRuntimeApi<B> + Send + Sync + 'static,
	CT: fp_rpc::ConvertTransaction<<B as BlockT>::Extrinsic> + Send + Sync + 'static,
	P: TransactionPool<Block = B> + Send + Sync + 'static,
	A: ChainApi<Block = B> + 'static,
{
	// ########################################################################
	// Client
	// ########################################################################

	/// Returns protocol version encoded as a string (quotes are necessary).
	fn protocol_version(&self) -> Result<u64> {
		Ok(1)
	}

	/// Returns an object with data about the sync status or false. (wtf?)
	fn syncing(&self) -> Result<SyncStatus> {
		if self.network.is_major_syncing() {
			let block_number = U256::from(UniqueSaturatedInto::<u128>::unique_saturated_into(
				self.client.info().best_number,
			));
			Ok(SyncStatus::Info(SyncInfo {
				starting_block: U256::zero(),
				current_block: block_number,
				// TODO `highest_block` is not correct, should load `best_seen_block` from
				// NetworkWorker, but afaik that is not currently possible in Substrate:
				// https://github.com/paritytech/substrate/issues/7311
				highest_block: block_number,
				warp_chunks_amount: None,
				warp_chunks_processed: None,
			}))
		} else {
			Ok(SyncStatus::None)
		}
	}

	/// Returns block author.
	fn author(&self) -> Result<H160> {
		Err(internal_err("author not supported"))
	}

	/// Returns accounts list.
	fn accounts(&self) -> Result<Vec<H160>> {
		Ok(vec![])
	}

	/// Returns highest block number.
	fn block_number(&self) -> Result<U256> {
		Ok(U256::from(UniqueSaturatedInto::<u128>::unique_saturated_into(
			self.client.info().best_number,
		)))
	}

	/// Returns the chain ID used for transaction signing at the
	/// current best block. None is returned if not
	/// available.
	fn chain_id(&self) -> Result<Option<U64>> {
		let at = BlockId::hash(self.client.info().best_hash);

		self.client
			.runtime_api()
			.chain_id(&at)
			.map(|v| Some(v.into()))
			.map_err(|err| internal_err(format!("fetch runtime chain id failed: {:?}", err)))
	}

	// ########################################################################
	// Block
	// ########################################################################

	/// Returns block with given hash.
	async fn block_by_hash(&self, hash: H256, full: bool) -> Result<Option<RichBlock>> {
		Err(internal_err("block_by_hash not supported"))
	}

	/// Returns block with given number.
	async fn block_by_number(&self, number: BlockNumber, full: bool) -> Result<Option<RichBlock>> {
		Err(internal_err("block_by_number not supported"))
	}

	/// Returns the number of transactions in a block with given hash.
	fn block_transaction_count_by_hash(&self, hash: H256) -> Result<Option<U256>> {
		Err(internal_err("block_transaction_count_by_hash not supported"))
	}

	/// Returns the number of transactions in a block with given block number.
	fn block_transaction_count_by_number(&self, number: BlockNumber) -> Result<Option<U256>> {
		Err(internal_err("block_transaction_count_by_number not supported"))
	}

	/// Returns the number of uncles in a block with given hash.
	fn block_uncles_count_by_hash(&self, hash: H256) -> Result<U256> {
		Ok(U256::zero())
	}

	/// Returns the number of uncles in a block with given block number.
	fn block_uncles_count_by_number(&self, number: BlockNumber) -> Result<U256> {
		Ok(U256::zero())
	}

	/// Returns an uncles at given block and index.
	fn uncle_by_block_hash_and_index(&self, hash: H256, index: Index) -> Result<Option<RichBlock>> {
		Ok(None)
	}

	/// Returns an uncles at given block and index.
	fn uncle_by_block_number_and_index(
		&self,
		number: BlockNumber,
		index: Index,
	) -> Result<Option<RichBlock>> {
		Ok(None)
	}

	// ########################################################################
	// Transaction
	// ########################################################################

	/// Get transaction by its hash.
	async fn transaction_by_hash(&self, hash: H256) -> Result<Option<Transaction>> {
		Err(internal_err("transaction_by_hash not supported"))
	}

	/// Returns transaction at given block hash and index.
	async fn transaction_by_block_hash_and_index(
		&self,
		hash: H256,
		index: Index,
	) -> Result<Option<Transaction>> {
		Err(internal_err("transaction_by_block_hash_and_index not supported"))
	}

	/// Returns transaction by given block number and index.
	async fn transaction_by_block_number_and_index(
		&self,
		number: BlockNumber,
		index: Index,
	) -> Result<Option<Transaction>> {
		Err(internal_err("transaction_by_block_number_and_index not supported"))
	}

	/// Returns transaction receipt by transaction hash.
	async fn transaction_receipt(&self, hash: H256) -> Result<Option<Receipt>> {
		Err(internal_err("transaction_receipt not supported"))
	}

	// ########################################################################
	// State
	// ########################################################################

	/// Returns balance of the given account.
	fn balance(&self, address: H160, number: Option<BlockNumber>) -> Result<U256> {
		let api = self.client.runtime_api();

		let mapper = BlockMapper::from_client(self.client.clone());

		if let Some(id) = mapper.map_block(number) {
			api.balances(&id, address)
				.map_err(|err| internal_err(format!("fetch runtime chain id failed: {:?}", err)))
		} else {
			let pending_api = pending_runtime_api(self.client.as_ref(), self.graph.as_ref())?;
			pending_api
				.balances(&BlockId::Hash(self.client.info().best_hash), address)
				.map_err(|err| internal_err(format!("fetch runtime chain id failed: {:?}", err)))
		}
	}

	/// Returns content of the storage at given address.
	fn storage_at(&self, address: H160, index: U256, number: Option<BlockNumber>) -> Result<H256> {
		let mapper = BlockMapper::from_client(self.client.clone());

		if let Some(id) = mapper.map_block(number) {
			let api = self.client.runtime_api();

			api.storage_at(&id, address, index)
				.map_err(|err| internal_err(format!("fetch runtime chain id failed: {:?}", err)))
		} else {
			let pending_api = pending_runtime_api(self.client.as_ref(), self.graph.as_ref())?;

			pending_api
				.storage_at(&BlockId::Hash(self.client.info().best_hash), address, index)
				.map_err(|err| internal_err(format!("fetch runtime chain id failed: {:?}", err)))
		}
	}

	/// Returns the number of transactions sent from given address at given time (block number).
	fn transaction_count(&self, address: H160, number: Option<BlockNumber>) -> Result<U256> {
		let mapper = BlockMapper::from_client(self.client.clone());

		if let Some(id) = mapper.map_block(number) {
			let api = self.client.runtime_api();
			api.account_nonce(&id, address)
				.map_err(|err| internal_err(format!("fetch runtime chain id failed: {:?}", err)))
		} else {
			let block = BlockId::Hash(self.client.info().best_hash);

			let nonce =
				self.client.runtime_api().account_nonce(&block, address).map_err(|err| {
					internal_err(format!("fetch runtime account basic failed: {:?}", err))
				})?;

			let mut current_nonce = nonce;
			let mut current_tag = (address, nonce).encode();
			for tx in self.pool.ready() {
				// since transactions in `ready()` need to be ordered by nonce
				// it's fine to continue with current iterator.
				if tx.provides().get(0) == Some(&current_tag) {
					current_nonce = current_nonce.saturating_add(1.into());
					current_tag = (address, current_nonce).encode();
				}
			}

			Ok(current_nonce)
		}
	}

	/// Returns the code at given address at given time (block number).
	fn code_at(&self, address: H160, number: Option<BlockNumber>) -> Result<Bytes> {
		Err(internal_err("code_at not supported"))
	}

	// ########################################################################
	// Execute
	// ########################################################################

	/// Call contract, returning the output data.
	fn call(&self, request: CallRequest, number: Option<BlockNumber>) -> Result<Bytes> {
		Err(internal_err("call not supported"))
	}

	/// Estimate gas needed for execution of given contract.
	async fn estimate_gas(
		&self,
		request: CallRequest,
		number: Option<BlockNumber>,
	) -> Result<U256> {
		Err(internal_err("estimate_gas not supported"))
	}

	// ########################################################################
	// Fee
	// ########################################################################

	/// Returns current gas_price.
	fn gas_price(&self) -> Result<U256> {
		Ok(Default::default())
	}

	/// Introduced in EIP-1159 for getting information on the appropriate priority fee to use.
	fn fee_history(
		&self,
		block_count: U256,
		newest_block: BlockNumber,
		reward_percentiles: Option<Vec<f64>>,
	) -> Result<FeeHistory> {
		Err(internal_err("fee_history not supported"))
	}

	/// Introduced in EIP-1159, a Geth-specific and simplified priority fee oracle.
	/// Leverages the already existing fee history cache.
	fn max_priority_fee_per_gas(&self) -> Result<U256> {
		Err(internal_err("max_priority_fee_per_gas not supported"))
	}

	// ########################################################################
	// Mining
	// ########################################################################

	/// Returns true if client is actively mining new blocks.
	fn is_mining(&self) -> Result<bool> {
		// unimplemented!()
		Ok(self.is_authority)
	}

	/// Returns the number of hashes per second that the node is mining with.
	fn hashrate(&self) -> Result<U256> {
		Ok(Default::default())
	}

	/// Returns the hash of the current block, the seedHash, and the boundary condition to be met.
	fn work(&self) -> Result<Work> {
		Ok(Default::default())
	}

	/// Used for submitting mining hashrate.
	fn submit_hashrate(&self, hashrate: U256, id: H256) -> Result<bool> {
		Ok(false)
	}

	/// Used for submitting a proof-of-work solution.
	fn submit_work(&self, nonce: H64, pow_hash: H256, mix_digest: H256) -> Result<bool> {
		Ok(false)
	}

	// ########################################################################
	// Submit
	// ########################################################################

	/// Sends transaction; will block waiting for signer to return the
	/// transaction hash.
	async fn send_transaction(&self, request: TransactionRequest) -> Result<H256> {
		Err(internal_err("send_transaction not supported"))
	}

	// NOTICE: eth-rpc expects encoding with rlp, where substrate defaults to scale-codec!!!
	/// Sends signed transaction, returning its hash.
	async fn send_raw_transaction(&self, bytes: Bytes) -> Result<H256> {
		let slice = &bytes.0[..];
		if slice.is_empty() {
			return Err(internal_err("transaction data is empty"))
		}
		let first = slice.first().unwrap();
		let transaction = if first > &0x7f {
			// Legacy transaction. Decode and wrap in envelope.
			match rlp::decode::<ethereum::TransactionV0>(slice) {
				Ok(transaction) => ethereum::TransactionV2::Legacy(transaction),
				Err(_) => return Err(internal_err("decode transaction failed")),
			}
		} else {
			// Typed Transaction.
			// `ethereum` crate decode implementation for `TransactionV2` expects a valid rlp input,
			// and EIP-1559 breaks that assumption by prepending a version byte.
			// We re-encode the payload input to get a valid rlp, and the decode implementation will
			// strip them to check the transaction version byte.
			let extend = rlp::encode(&slice);
			match rlp::decode::<ethereum::TransactionV2>(&extend[..]) {
				Ok(transaction) => transaction,
				Err(_) => return Err(internal_err("decode transaction failed")),
			}
		};

		let transaction_hash = transaction.hash();

		let block_hash = BlockId::hash(self.client.info().best_hash);
		let api_version = match self
			.client
			.runtime_api()
			.api_version::<dyn ConvertTransactionRuntimeApi<B>>(&block_hash)
		{
			Ok(api_version) => api_version,
			_ => return Err(internal_err("cannot access runtime api")),
		};

		let extrinsic = match api_version {
			Some(2) =>
				match self.client.runtime_api().convert_transaction(&block_hash, transaction) {
					Ok(extrinsic) => extrinsic,
					Err(_) => return Err(internal_err("cannot access runtime api")),
				},
			Some(1) => {
				if let ethereum::TransactionV2::Legacy(legacy_transaction) = transaction {
					// To be compatible with runtimes that do not support transactions v2
					#[allow(deprecated)]
					match self
						.client
						.runtime_api()
						.convert_transaction_before_version_2(&block_hash, legacy_transaction)
					{
						Ok(extrinsic) => extrinsic,
						Err(_) => return Err(internal_err("cannot access runtime api")),
					}
				} else {
					return Err(internal_err("This runtime not support eth transactions v2"))
				}
			},
			None =>
				if let Some(ref convert_transaction) = self.convert_transaction {
					convert_transaction.convert_transaction(transaction.clone())
				} else {
					return Err(internal_err(
						"No TransactionConverter is provided and the runtime api ConvertTransactionRuntimeApi is not found"
					));
				},
			_ => return Err(internal_err("ConvertTransactionRuntimeApi version not supported")),
		};

		self.pool
			.submit_one(&block_hash, TransactionSource::Local, extrinsic)
			.await
			.map(move |_| transaction_hash)
			.map_err(|err| internal_err(format::Geth::pool_error(err)))
	}
}
