pub mod forked_db;

use ethers::{prelude::*, abi::{parse_abi, Abi}, utils::{parse_ether, keccak256}};
use ethabi::Token;
use std::sync::Arc;
use std::str::FromStr;
use forked_db::{*, fork_factory::ForkFactory, fork_db::ForkDB};

use revm::primitives::{Bytecode, Bytes as rBytes, Address as rAddress, B256, AccountInfo, TransactTo, Log};
use revm::Evm;
use bigdecimal::BigDecimal;
use lazy_static::lazy_static;
use serde_json::Value;


lazy_static!{
    pub static ref WETH: Address = Address::from_str("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2").unwrap();
    pub static ref USDT: Address = Address::from_str("0xdAC17F958D2ee523a2206206994597C13D831ec7").unwrap();
    pub static ref USDC: Address = Address::from_str("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48").unwrap();
}

/// Parameters used for a swap
#[derive(Debug, Clone)]
pub struct SwapParams {
    pub input_token: Address,
    pub output_token: Address,
    pub amount_in: U256,
    pub pool: Address,
    pub pool_variant: U256,
    pub minimum_received: U256,
}

impl SwapParams {
    pub fn to_tokens(&self) -> Vec<Token> {
        vec![
            Token::Tuple(
                vec![
                    Token::Address(self.input_token),
                    Token::Address(self.output_token),
                    Token::Uint(self.amount_in),
                    Token::Address(self.pool),
                    Token::Uint(self.pool_variant),
                    Token::Uint(self.minimum_received)
                ]
            )
        ]
    }
}

/// EOA (Externally Owned Account)
/// 
/// Contract (An Ethereum Smart Contract)
pub enum AccountType {
    EOA,
    Contract
}

/// Struct that holds the Parameters used for [sim_call]
/// 
/// ## Arguments
/// 
/// - `caller`: The address of the caller
/// 
/// - `transact_to`: The address of the contract to interact with
/// 
/// - `call_data`: The call data to send to the contract
/// 
/// - `value`: The amount of ETH to send with the transaction
/// 
/// - `apply_changes`: Whether to apply the state changes or not to [Evm]
/// 
/// - `evm`: The [Evm] instance to use
#[derive(Debug)]
pub struct EvmParams {
    pub caller: Address,
    pub transact_to: Address,
    pub call_data: Bytes,
    pub value: U256,
    pub apply_changes: bool,
    pub evm: Evm<'static, (), ForkDB>
}

impl EvmParams {
    /// Sets the transaction environment for the [Evm] instance
    pub fn set_tx_env(&mut self) {
        self.evm.tx_mut().caller = self.caller.0.into();
        self.evm.tx_mut().transact_to = TransactTo::Call(self.transact_to.0.into());
        self.evm.tx_mut().data = rBytes::from(self.call_data.clone().0);
        self.evm.tx_mut().value = to_revm_u256(self.value);
    }

    /// Sets the `caller` of the transaction
    pub fn set_caller(&mut self, caller: Address) {
        self.caller = caller;
    }

    /// Sets the `transact_to` address
    pub fn set_transact_to(&mut self, transact_to: Address) {
        self.transact_to = transact_to;
    }

    /// Sets the `call_data`
    pub fn set_call_data(&mut self, call_data: Bytes) {
        self.call_data = call_data;
    }

    /// Sets the `value` of the transaction
    pub fn set_value(&mut self, value: U256) {
        self.value = value;
    }

    /// Sets whether to apply the changes or not
    pub fn set_apply_changes(&mut self, apply_changes: bool) {
        self.apply_changes = apply_changes;
    }

    /// Sets the [Evm] instance
    pub fn set_evm(&mut self, evm: Evm<'static, (), ForkDB>) {
        self.evm = evm;
    }
}

/// Struct that holds the result of a simulation
/// 
/// ## Fields
/// 
/// - `is_reverted`: Whether the call was reverted or not
/// 
/// - `logs`: The logs produced by the call
/// 
/// - `gas_used`: The amount of gas was used
/// 
/// - `output`: The output of the call (If the function of the contract returns a value)
#[derive(Debug, Clone)]
pub struct SimulationResult {
    pub is_reverted: bool,
    pub logs: Vec<Log>,
    pub gas_used: u64,
    pub output: rBytes,
}


#[derive(Debug, Clone)]
pub struct Pool {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub variant: PoolVariant,
}

impl Pool {
    pub fn variant(&self) -> U256 {
        match self.variant {
            PoolVariant::UniswapV2 => U256::zero(),
            PoolVariant::UniswapV3 => U256::one(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum PoolVariant {
    UniswapV2,
    UniswapV3
}




pub async fn get_client() -> Result<Arc<Provider<Ws>>, anyhow::Error> {
    let url: &str = "wss://eth.merkle.io";
    let client = Provider::<Ws>::connect(url).await?;
    Ok(Arc::new(client))
}



/// Creates a new [Evm] instance with initial state from [ForkDB]
/// 
/// State changes are applied to [Evm]
pub fn new_evm(fork_db: ForkDB, block: Block<H256>) -> Evm<'static, (), ForkDB> {
    let mut evm = Evm::builder().with_db(fork_db).build();

    let evm_block = U256::from(block.number.unwrap().as_u64());

    evm.block_mut().number = to_revm_u256(evm_block);
    evm.block_mut().timestamp = to_revm_u256(block.timestamp);
    evm.block_mut().coinbase = rAddress
        ::from_str("0xDecafC0FFEe15BAD000000000000000000000000")
        .unwrap();
    
    // Disable some checks for easier testing
    evm.cfg_mut().disable_balance_check = true;
    evm.cfg_mut().disable_block_gas_limit = true;
    evm.cfg_mut().disable_base_fee = true;
    evm
}



/// Simulates a call with the given [EvmParams]
/// 
/// ## Returns
///
/// [SimulationResult]
pub fn sim_call(params: &mut EvmParams) -> Result<SimulationResult, anyhow::Error> {
    params.set_tx_env();


   let result = if params.apply_changes {
        params.evm.transact_commit()?
    } else {
        params.evm.transact()?.result
    };

    let is_reverted = match_output_reverted(&result);
    let logs = result.logs().to_vec();
    let gas_used = result.gas_used();
    let output = result.into_output().unwrap_or_default();

    let sim_result = SimulationResult {
        is_reverted,
        logs,
        gas_used,
        output,
    };

    Ok(sim_result)
}

/// Encodes the swap parameters needed for the swap function of the router contract
pub fn encode_swap(params: SwapParams) -> Vec<u8> {
    let contract_abi = swap_router_abi();
    let swap_abi = contract_abi.function("do_swap").unwrap();
    let tokens = params.to_tokens();
    let encoded_args = swap_abi.encode_input(&tokens).unwrap();
    encoded_args
}

/// Decodes the output of the swap function of the router contract
/// 
/// ## Returns
/// [U256] the real amount received after the swap
pub fn decode_swap(bytes: Bytes) -> Result<U256, anyhow::Error> {
    let tokens = swap_router_abi().function("do_swap").unwrap().decode_output(&bytes)?;

    if let Some(Token::Uint(value)) = tokens.get(0) {
        Ok(value.clone())
    } else {
        Err(anyhow::anyhow!("Error decoding amount"))
    }
}

pub fn encode_recover_erc20(
    token: Address,
    amount: U256
) -> Vec<u8> {
    let method_id = &keccak256(b"recover_erc20(address,uint256)")[0..4];
    
    let encoded_args = ethabi::encode(
        &[
            ethabi::Token::Address(token),
            ethabi::Token::Uint(amount),
        ]
    );

    let mut payload = vec![];
    payload.extend_from_slice(method_id);
    payload.extend_from_slice(&encoded_args);

    payload
}

/// ERC20 approve function
pub fn encode_approve(spender: Address, amount: U256) -> Vec<u8> {
    let method_id = &keccak256(b"approve(address,uint256)")[0..4];

    let encoded_args = ethabi::encode(
        &[ethabi::Token::Address(spender), ethabi::Token::Uint(amount)]
    );

    let mut payload = vec![];
    payload.extend_from_slice(method_id);
    payload.extend_from_slice(&encoded_args);

    payload
}

/// ERC20 transfer function
pub fn encode_transfer(
    recipient: Address,
    amount: U256,
) -> Vec<u8> {
    let method_id = &keccak256(b"transfer(address,uint256)")[0..4];
    
    let encoded_args = ethabi::encode(
        &[
            ethabi::Token::Address(recipient),
            ethabi::Token::Uint(amount),
        ]
    );

    let mut payload = vec![];
    payload.extend_from_slice(method_id);
    payload.extend_from_slice(&encoded_args);

    payload
}


/// Inserts a dummy account to the local fork enviroment
pub fn insert_dummy_account(account_type: AccountType, fork_factory: &mut ForkFactory) -> Result<Address, anyhow::Error> {

    // generate a random address
    let dummy_account = LocalWallet::new(&mut rand::thread_rng());

    let (balance, code) = match account_type {
        AccountType::EOA => (parse_ether(1)?, None),
        AccountType::Contract => (U256::zero(), Some(Bytecode::new_raw(rBytes::from(get_bytecode().0)))
        )
    };

    let account_info = AccountInfo {
        balance: to_revm_u256(balance),
        nonce: 0,
        code_hash: B256::default(),
        code
    };

    // insert the account info into the fork enviroment
    fork_factory.insert_account_info(dummy_account.address().0.into(), account_info);

    // Now we fund the dummy account with 1 WETH
    let weth_amount = parse_ether(1).unwrap();

    // To fund any ERC20 token to an account we need the balance storage slot of the token
    // For WETH its 3
    // An amazing online tool to see the storage mapping of any contract https://evm.storage/
    let weth_slot: U256 = keccak256(abi::encode(&[
        abi::Token::Address(dummy_account.address().0.into()),
        abi::Token::Uint(U256::from(3)),
    ])).into();

    // insert the erc20 token balance to the dummy account
    if let Err(e) = fork_factory.insert_account_storage(
        WETH.0.into(),
        to_revm_u256(weth_slot),
        to_revm_u256(weth_amount),
    ) {
        return Err(anyhow::anyhow!("Failed to insert account storage: {}", e));
    }

    Ok(dummy_account.address())
}



pub fn to_readable(amount: U256, token: Address) -> String {
    let decimals = match_decimals(token);
    let divisor_str = format!("1{:0>width$}", "", width = decimals as usize);
    let divisor = BigDecimal::from_str(&divisor_str).unwrap();
    let amount_as_decimal = BigDecimal::from_str(&amount.to_string()).unwrap();
    let amount = amount_as_decimal / divisor;
    let token = match token {
        t if t == *WETH => "WETH",
        t if t == *USDT => "USDT",
        t if t == *USDC => "USDC",
        _ => "Token"
    };
    format!("{:.4} {}", amount, token)
}

pub fn match_decimals(token: Address) -> u32 {
    match token {
       t if t == *WETH => 18,
       t if t == *USDT => 6,
       t if t == *USDC => 6,
        _ => 18
    }
}

// Deployed Bytecode of swap router contract
fn get_bytecode() -> Bytes {
    "0x608060408181526004908136101561001657600080fd5b600092833560e01c90816323a50d3c14610a2757508063ac20c2ca146102125763fa461e331461004557600080fd5b3461020e57606036600319011261020e576044359067ffffffffffffffff80831161020657366023840112156102065782840135908111610206578201366024820111610206578260a091031261020a5760248201358015158103610206576100b060448401610abe565b906100bd60648501610abe565b9260a46100cc60848701610abe565b9501359262ffffff84168094036101f0576001600160a01b0380809216961693818351967f1698ee82000000000000000000000000000000000000000000000000000000008852888a8901521660248701526044860152602085606481731f98431c8ad98523631ae4a59f267346ea31f9845afa9485156101fc5788956101bb575b508416330361017957501561016b57610168933592610b6f565b80f35b610168935060243592610b6f565b5162461bcd60e51b8152602081870152600c60248201527f4e6f742074686520706f6f6c00000000000000000000000000000000000000006044820152606490fd5b9094506020813d6020116101f4575b816101d760209383610ad2565b810103126101f0575184811681036101f057933861014e565b8780fd5b3d91506101ca565b82513d8a823e3d90fd5b8480fd5b8380fd5b8280fd5b503461020e5760c036600319011261020e576001600160a01b039182610236610b0a565b16938251809581957f70a082310000000000000000000000000000000000000000000000000000000091828452338685015260209889916024998a915afa928315610a1d5784936109ee575b506084358061076657506102a681610298610b20565b166044359030903390610b6f565b846060826102b2610b36565b168851928380927f0902f1ac0000000000000000000000000000000000000000000000000000000082525afa90811561075c5785908692610702575b506dffffffffffffffffffffffffffff918216911661030b610b20565b8380610315610b0a565b16911610156106fd57905b82610329610b20565b168a848b610335610b36565b8c5194859384928b8452168d8301525afa80156106f357839088906106be575b61035f9250610b4c565b801561065757821580158061064e575b156105e7576103e58083029280840482036105d5578402029282840414821517156105c3576103e88085029485041417156105b157820180921161059f57811561058d57046103bc610b20565b82806103c6610b0a565b169116101561058657845b826103da610b36565b168851918b83019267ffffffffffffffff9381811085821117610574578b52888152823b1561057057918b91898b61045682968f51998a97889687957f022c0d9f000000000000000000000000000000000000000000000000000000008752860152840152336044840152608060648401526084830190610cd5565b03925af1801561056657908a93929161053d575b5050610474610b0a565b169187875180948193825233898301525afa908115610533578391610501575b50915b5080156104fa576104a791610b4c565b925b60a43584106104ba57505051908152f35b601e9085606494519362461bcd60e51b85528401528201527f5265616c20416d6f756e74203c204d696e696d756d20526563656976656400006044820152fd5b50926104a9565b90508681813d831161052c575b6105188183610ad2565b81010312610527575138610494565b600080fd5b503d61050e565b85513d85823e3d90fd5b90809296935011610554578652928790388061046a565b8782604188634e487b7160e01b835252fd5b88513d88823e3d90fd5b8880fd5b8c8a60418d634e487b7160e01b835252fd5b84906103d1565b8886601289634e487b7160e01b835252fd5b8886601189634e487b7160e01b835252fd5b898760118a634e487b7160e01b835252fd5b8a8860118b634e487b7160e01b835252fd5b8c8a60118d634e487b7160e01b835252fd5b60848960288d8f8e519362461bcd60e51b85528401528201527f556e697377617056324c6962726172793a20494e53554646494349454e545f4c60448201527f49515549444954590000000000000000000000000000000000000000000000006064820152fd5b5082151561036f565b608488602b8c8e8d519362461bcd60e51b85528401528201527f556e697377617056324c6962726172793a20494e53554646494349454e545f4960448201527f4e5055545f414d4f554e540000000000000000000000000000000000000000006064820152fd5b50508a81813d83116106ec575b6106d58183610ad2565b810103126106e8578261035f9151610355565b8680fd5b503d6106cb565b89513d89823e3d90fd5b610320565b9150506060813d606011610754575b8161071e60609383610ad2565b810103126102065761072f81610d15565b8761073b8b8401610d15565b92015163ffffffff81160361075057386102ee565b8580fd5b3d9150610711565b87513d87823e3d90fd5b6001036109ad57610775610b20565b818061077f610b0a565b16911610801561099257856401000276ad5b8a8461079b610b36565b168a51938480927fddca3f430000000000000000000000000000000000000000000000000000000082525afa9182156106f357908b9392918892610954575b5062ffffff856107e8610b36565b1692866107f3610b20565b8d826107fd610b0a565b928983519b8c015216908901521660608701523360808701521660a085015260a0845260c084019284841067ffffffffffffffff85111761094257838b527f128acb080000000000000000000000000000000000000000000000000000000084523360c486015260e4850152604435610104850152841661012484015260a061014484015288908290818960bf198761089a610164820182610cd5565b0301925af18015610566579188918b949361090f575b5050506108bb610b0a565b169187875180948193825233898301525afa9081156105335783916108e2575b5091610497565b90508681813d8311610908575b6108f98183610ad2565b8101031261020e5751386108db565b503d6108ef565b909180939450903d841161093a575b8161092891610ad2565b8101031261020a5787908638806108b0565b3d915061091e565b8b8960418c634e487b7160e01b835252fd5b809250849193943d831161098b575b61096d8183610ad2565b810103126106e8575162ffffff811681036106e8578a9291386107da565b503d610963565b8573fffd8963efd1fc6a506488495d951d5263988d25610791565b6064856014898b8a519362461bcd60e51b85528401528201527f496e76616c696420706f6f6c2076617269616e740000000000000000000000006044820152fd5b9092508781813d8311610a16575b610a068183610ad2565b8101031261020a57519138610282565b503d6109fc565b86513d86823e3d90fd5b905083913461020e578060031936011261020e578335906001600160a01b03821680920361020a577fa9059cbb000000000000000000000000000000000000000000000000000000006020840152336024840152602435604484015260448352608083019083821067ffffffffffffffff831117610aab5761016894955052610be0565b602485604188634e487b7160e01b835252fd5b35906001600160a01b038216820361052757565b90601f8019910116810190811067ffffffffffffffff821117610af457604052565b634e487b7160e01b600052604160045260246000fd5b6024356001600160a01b03811681036105275790565b6004356001600160a01b03811681036105275790565b6064356001600160a01b03811681036105275790565b91908203918211610b5957565b634e487b7160e01b600052601160045260246000fd5b9290604051927f23b872dd0000000000000000000000000000000000000000000000000000000060208501526001600160a01b03809216602485015216604483015260648201526064815260a081019181831067ffffffffffffffff841117610af457610bde92604052610be0565b565b6001600160a01b031690600080826020829451910182865af13d15610cc9573d9067ffffffffffffffff8211610cb55790610c3d9160405191610c2d6020601f19601f8401160184610ad2565b82523d84602084013e5b84610d30565b908151918215159283610c86575b505050610c555750565b602490604051907f5274afe70000000000000000000000000000000000000000000000000000000082526004820152fd5b819293509060209181010312610cb1576020015190811591821503610cae5750388080610c4b565b80fd5b5080fd5b602483634e487b7160e01b81526041600452fd5b610c3d90606090610c37565b919082519283825260005b848110610d01575050826000602080949584010152601f8019910116010190565b602081830181015184830182015201610ce0565b51906dffffffffffffffffffffffffffff8216820361052757565b90610d6f5750805115610d4557805190602001fd5b60046040517f1425ea42000000000000000000000000000000000000000000000000000000008152fd5b81511580610dba575b610d80575090565b6024906001600160a01b03604051917f9996b315000000000000000000000000000000000000000000000000000000008352166004820152fd5b50803b15610d7856fea26469706673582212207dff05993375a2f7baff60fb7f3a4d3baf3b9e97af56129124b1797f380c3ecb64736f6c63430008170033"
    .parse()
    .unwrap()
}


// ** ABI getters

pub fn swap_router_abi() -> Abi {
    let json = include_str!("../contracts/SwapRouter.json");
    let value: Value = serde_json::from_str(json).unwrap();
    serde_json::from_value(value["abi"].clone()).unwrap()
}

pub fn weth_deposit() -> BaseContract {
    BaseContract::from(parse_abi(
        &["function deposit() public payable"]
    ).unwrap())
}

pub fn erc20_balanceof() -> BaseContract {
    BaseContract::from(parse_abi(
        &["function balanceOf(address) public view returns (uint256)"]
    ).unwrap())
}