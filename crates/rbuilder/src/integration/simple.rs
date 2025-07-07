#[cfg(test)]
mod tests {
    use crate::{
        integration::playground::Playground,
        live_builder::block_list_provider::test::{BlocklistHttpServer, BLOCKLIST_LEN_2},
    };

    use alloy_network::TransactionBuilder;
    use alloy_primitives::U256;
    use alloy_provider::{PendingTransactionBuilder, Provider, ProviderBuilder};
    use alloy_rpc_types::TransactionRequest;
    use alloy_signer_local::PrivateKeySigner;
    use std::{path::PathBuf, str::FromStr, time::Duration};
    use test_utils::ignore_if_env_not_set;
    use url::Url;

    async fn send_transaction(
        srv: &Playground,
        private_key: alloy_network::EthereumWallet,
        to: Option<alloy_primitives::Address>,
    ) -> eyre::Result<alloy_primitives::TxHash> {
        let rbuilder_provider =
            ProviderBuilder::new().on_http(Url::parse(srv.rbuilder_rpc_url()).unwrap());

        let provider = ProviderBuilder::new()
            .wallet(private_key.clone())
            .on_http(Url::parse(srv.el_url()).unwrap());

        let gas_price = provider.get_gas_price().await?;
        let account_nonce = provider.get_transaction_count(private_key.default_signer().address()).await?;

        println!("Account nonce!!! :{:?}", account_nonce);

        let tx = TransactionRequest::default()
            .with_nonce(account_nonce)
            .with_to(to.unwrap_or(srv.builder_address()))
            .with_value(U256::from_str("1000000000000000000").unwrap())
            .with_gas_price(gas_price)
            .with_gas_limit(21000);

        let tx = provider.fill(tx).await?;

        // send the transaction ONLY to the builder
        let pending_tx = rbuilder_provider
            .send_tx_envelope(tx.as_envelope().unwrap().clone())
            .await?;

        let tx = TransactionRequest::default()
            .with_nonce(account_nonce + 1)
            .with_to(to.unwrap_or(srv.builder_address()))
            .with_value(U256::from_str("2000000000000000000").unwrap())
            .with_gas_price(gas_price)
            .with_gas_limit(21000);

        let tx = provider.fill(tx).await?;

        // send the transaction ONLY to the builder
        let pending_tx = rbuilder_provider
            .send_tx_envelope(tx.as_envelope().unwrap().clone())
            .await?;

        let tx = TransactionRequest::default()
            .with_nonce(account_nonce + 2)
            .with_to(to.unwrap_or(srv.builder_address()))
            .with_value(U256::from_str("3000000000000000000").unwrap())
            .with_gas_price(gas_price)
            .with_gas_limit(21000);

        let tx = provider.fill(tx).await?;

        // send the transaction ONLY to the builder
        let pending_tx = rbuilder_provider
            .send_tx_envelope(tx.as_envelope().unwrap().clone())
            .await?;

        let tx = TransactionRequest::default()
            .with_nonce(account_nonce + 3)
            .with_to(to.unwrap_or(srv.builder_address()))
            .with_value(U256::from_str("4000000000000000000").unwrap())
            .with_gas_price(gas_price)
            .with_gas_limit(21000);

        let tx = provider.fill(tx).await?;

        // send the transaction ONLY to the builder
        let pending_tx = rbuilder_provider
            .send_tx_envelope(tx.as_envelope().unwrap().clone())
            .await?;

        let tx = TransactionRequest::default()
            .with_nonce(account_nonce + 4)
            .with_to(to.unwrap_or(srv.builder_address()))
            .with_value(U256::from_str("5000000000000000000").unwrap())
            .with_gas_price(gas_price)
            .with_gas_limit(21000);

        let tx = provider.fill(tx).await?;

        // send the transaction ONLY to the builder
        let pending_tx = rbuilder_provider
            .send_tx_envelope(tx.as_envelope().unwrap().clone())
            .await?;

        let tx = TransactionRequest::default()
            .with_nonce(account_nonce + 5)
            .with_to(to.unwrap_or(srv.builder_address()))
            .with_value(U256::from_str("6000000000000000000").unwrap())
            .with_gas_price(gas_price)
            .with_gas_limit(21000);

        let tx = provider.fill(tx).await?;

        // send the transaction ONLY to the builder
        let pending_tx = rbuilder_provider
            .send_tx_envelope(tx.as_envelope().unwrap().clone())
            .await?;

        let tx = TransactionRequest::default()
            .with_nonce(account_nonce + 6)
            .with_to(to.unwrap_or(srv.builder_address()))
            .with_value(U256::from_str("7000000000000000000").unwrap())
            .with_gas_price(gas_price)
            .with_gas_limit(21000);

        let tx = provider.fill(tx).await?;

        // send the transaction ONLY to the builder
        let pending_tx = rbuilder_provider
            .send_tx_envelope(tx.as_envelope().unwrap().clone())
            .await?;

        Ok(*pending_tx.tx_hash())
    }

    async fn send_transactions_multiple_accounts(
        srv: &Playground,
        to: Option<alloy_primitives::Address>,
    ) -> eyre::Result<Vec<alloy_primitives::TxHash>> {
        let rbuilder_provider =
            ProviderBuilder::new().on_http(Url::parse(srv.rbuilder_rpc_url()).unwrap());

        let accounts = vec![
            {
                let signer: PrivateKeySigner =
                    "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
                        .parse()
                        .unwrap();
                alloy_network::EthereumWallet::from(signer)
            },
            {
                let signer: PrivateKeySigner =
                    "7c852118294e51e653712a81e05800f419141751be58f605c371e15141b007a6"
                        .parse()
                        .unwrap();
                alloy_network::EthereumWallet::from(signer)
            },
            {
                let signer: PrivateKeySigner =
                    "47e179ec197488593b187f80a00eb0da91f1b9d0b13f8733639f19c30a34926a"
                        .parse()
                        .unwrap();
                alloy_network::EthereumWallet::from(signer)
            },
        ];

        let mut tx_hashes = Vec::new();

        for (account_idx, account) in accounts.iter().enumerate() {
            let provider = ProviderBuilder::new()
                .wallet(account.clone())
                .on_http(Url::parse(srv.el_url()).unwrap());

            let gas_price = provider.get_gas_price().await?;
            let account_nonce = provider.get_transaction_count(account.default_signer().address()).await?;

            println!("Account {} nonce: {:?}", account_idx + 1, account_nonce);

            let num_txs = if account_idx == 0 { 3 } else { 2 };
            
            for tx_idx in 0..num_txs {
                let value = U256::from_str(&format!(
                    "{}000000000000000000", 
                    account_idx * 10 + tx_idx + 1
                )).unwrap();
                
                let tx = TransactionRequest::default()
                    .with_nonce(account_nonce + tx_idx as u64)
                    .with_to(to.unwrap_or(srv.builder_address()))
                    .with_value(value)
                    .with_gas_price(gas_price)
                    .with_gas_limit(21000);

                let tx = provider.fill(tx).await?;

                let pending_tx = rbuilder_provider
                    .send_tx_envelope(tx.as_envelope().unwrap().clone())
                    .await?;
                
                tx_hashes.push(*pending_tx.tx_hash());
                
                println!("Sent transaction {} from account {}: {:?}", tx_idx + 1, account_idx + 1, pending_tx.tx_hash());
            }
        }

        Ok(tx_hashes)
    }

    #[ignore_if_env_not_set("PLAYGROUND")] // TODO: Change with a custom macro (i.e ignore_if_not_playground)
    #[tokio::test]
    async fn test_simple_example() {
        const USE_MULTIPLE_ACCOUNTS: bool = true;
        
        let config_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../config-wasm-playground.toml");

        // This test sends a transaction ONLY to the builder and waits for the block to be built with it.
        let srv = Playground::new("test_simple_example", &config_path).unwrap();
        srv.wait_for_next_slot().await.unwrap();

        let binding = ProviderBuilder::new().on_http(Url::parse(srv.el_url()).unwrap());
        
        if USE_MULTIPLE_ACCOUNTS {
            println!("Using multiple accounts mode");
            let tx_hashes = send_transactions_multiple_accounts(&srv, None)
                .await
                .unwrap();
            
            let first_tx_hash = tx_hashes[0];
            let pending_tx = PendingTransactionBuilder::new(binding.root().clone(), first_tx_hash)
                .with_timeout(Some(std::time::Duration::from_secs(60)));

            let receipt = pending_tx.get_receipt().await.unwrap();
            srv.validate_block_built(receipt.block_number.unwrap())
                .await
                .unwrap();
        } else {
            println!("Using single account with multiple transactions mode");
            let tx_hash = send_transaction(&srv, srv.prefunded_key(), None)
                .await
                .unwrap();

            let pending_tx = PendingTransactionBuilder::new(binding.root().clone(), tx_hash)
                .with_timeout(Some(std::time::Duration::from_secs(60)));

            let receipt = pending_tx.get_receipt().await.unwrap();
            srv.validate_block_built(receipt.block_number.unwrap())
                .await
                .unwrap();
        }

        // Send a transaction with an account from the blocklist
        // TODO: This should be a separated test but the integration framework does use fixed port numbers
        // and we need to change it to use dynamic ports.
        // Since we only send the transaction to the builder, it should never be included in the block.
        {
            srv.wait_for_next_slot().await.unwrap();
            let tx_hash = if USE_MULTIPLE_ACCOUNTS {
                let rbuilder_provider =
                    ProviderBuilder::new().on_http(Url::parse(srv.rbuilder_rpc_url()).unwrap());
                let provider = ProviderBuilder::new()
                    .wallet(srv.blocklist_key())
                    .on_http(Url::parse(srv.el_url()).unwrap());
                let gas_price = provider.get_gas_price().await.unwrap();
                let account_nonce = provider.get_transaction_count(srv.blocklist_key().default_signer().address()).await.unwrap();
                let tx = TransactionRequest::default()
                    .with_nonce(account_nonce)
                    .with_to(srv.builder_address())
                    .with_value(U256::from_str("1000000000000000000").unwrap())
                    .with_gas_price(gas_price)
                    .with_gas_limit(21000);
                let tx = provider.fill(tx).await.unwrap();
                let pending_tx = rbuilder_provider
                    .send_tx_envelope(tx.as_envelope().unwrap().clone())
                    .await.unwrap();
                *pending_tx.tx_hash()
            } else {
                send_transaction(&srv, srv.blocklist_key(), None)
                    .await
                    .unwrap()
            };

            // wait for 20 seconds
            let pending_tx = PendingTransactionBuilder::new(binding.root().clone(), tx_hash)
                .with_timeout(Some(std::time::Duration::from_secs(20)));

            assert!(
                pending_tx.get_receipt().await.is_err(),
                "Expected transaction to fail since account is blocklisted"
            );
        }

        // Second blocklist test, send a transaction from a non-blocklisted account to a blocklisted account
        {
            srv.wait_for_next_slot().await.unwrap();
            let tx_hash = if USE_MULTIPLE_ACCOUNTS {
                let rbuilder_provider =
                    ProviderBuilder::new().on_http(Url::parse(srv.rbuilder_rpc_url()).unwrap());
                let signer: PrivateKeySigner =
                    "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
                        .parse()
                        .unwrap();
                let wallet = alloy_network::EthereumWallet::from(signer);
                let provider = ProviderBuilder::new()
                    .wallet(wallet.clone())
                    .on_http(Url::parse(srv.el_url()).unwrap());
                let gas_price = provider.get_gas_price().await.unwrap();
                let account_nonce = provider.get_transaction_count(wallet.default_signer().address()).await.unwrap();
                let tx = TransactionRequest::default()
                    .with_nonce(account_nonce)
                    .with_to(srv.blocklist_address())
                    .with_value(U256::from_str("1000000000000000000").unwrap())
                    .with_gas_price(gas_price)
                    .with_gas_limit(21000);
                let tx = provider.fill(tx).await.unwrap();
                let pending_tx = rbuilder_provider
                    .send_tx_envelope(tx.as_envelope().unwrap().clone())
                    .await.unwrap();
                *pending_tx.tx_hash()
            } else {
                send_transaction(&srv, srv.prefunded_key(), Some(srv.blocklist_address()))
                    .await
                    .unwrap()
            };

            // wait for 20 seconds
            let pending_tx = PendingTransactionBuilder::new(binding.root().clone(), tx_hash)
                .with_timeout(Some(std::time::Duration::from_secs(20)));

            assert!(
                pending_tx.get_receipt().await.is_err(),
                "Expected transaction to fail since account is blocklisted"
            );
        }
    }

}
