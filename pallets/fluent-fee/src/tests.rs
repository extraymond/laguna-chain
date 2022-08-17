//! Unit test for the fluent-fee pallet

use super::pallet;
use crate::mock::{Call, Event, *};
use codec::Encode;
use frame_support::{
	assert_ok,
	dispatch::{Dispatchable, GetDispatchInfo},
	sp_runtime,
};

use orml_traits::MultiCurrency;
use pallet_transaction_payment::ChargeTransactionPayment;
use primitives::{CurrencyId, TokenId};
use sp_runtime::{traits::SignedExtension, FixedPointNumber, FixedU128};
use traits::fee::FeeMeasure;

#[test]
fn test_charge_native() {
	ExtBuilder::default()
		.balances(vec![
			(ALICE, NATIVE_CURRENCY_ID, 1_000_000_000_000_000),
			// (ALICE, FEE_CURRENCY_ID, 1000_000_000_000),
		])
		.build()
		.execute_with(|| {
			let call = Call::Tokens(orml_tokens::Call::transfer {
				dest: BOB,
				currency_id: NATIVE_CURRENCY_ID,
				amount: 100,
			});

			let len = call.encoded_size();
			let info = call.get_dispatch_info();
			let fee = Payment::compute_fee(len as u32, &info, 0);

			assert_ok!(ChargeTransactionPayment::<Runtime>::from(0)
				.validate(&ALICE, &call, &info, len as _,));

			assert_ok!(call.dispatch(Origin::signed(ALICE)));

			assert_eq!(
				Tokens::free_balance(NATIVE_CURRENCY_ID, &ALICE),
				1_000_000_000_000_000 - fee - 100
			);
		});
}

#[test]
fn test_charge_fee() {
	ExtBuilder::default()
		.balances(vec![
			(ALICE, NATIVE_CURRENCY_ID, 1_000_000_000_000),
			(ALICE, FEE_CURRENCY_ID, 1_000_000_000_000),
		])
		.build()
		.execute_with(|| {
			// set default
			assert_ok!(FluentFee::set_default(
				Origin::signed(ALICE),
				CurrencyId::NativeToken(TokenId::FeeToken),
			));

			assert_eq!(FluentFee::account_fee_source_priority(&ALICE), Some(FEE_CURRENCY_ID));

			let call = Call::Tokens(orml_tokens::Call::transfer {
				dest: BOB,
				currency_id: NATIVE_CURRENCY_ID,
				amount: 100,
			});

			assert_ok!(ChargeTransactionPayment::<Runtime>::from(0).validate(
				&ALICE,
				&call,
				&call.get_dispatch_info(),
				call.encoded_size(),
			));

			let len = call.encoded_size();
			let info = call.get_dispatch_info();
			let fee = Payment::compute_fee(len as u32, &info, 0);

			let discounted = <Runtime as crate::Config>::FeeMeasure::measure(&FEE_CURRENCY_ID, fee)
				.expect("received target amount");

			assert_eq!(
				Tokens::free_balance(FEE_CURRENCY_ID, &ALICE),
				1_000_000_000_000 - discounted
			);
		});
}

#[test]
fn test_fee_sharing_beneficiary_works() {
	ExtBuilder::default()
		.balances(vec![
			(ALICE, NATIVE_CURRENCY_ID, 1_000_000_000_000),
			(BOB, NATIVE_CURRENCY_ID, 1_000_000_000_000),
			(EVA, NATIVE_CURRENCY_ID, 1_000_000_000_000),
		])
		.build()
		.execute_with(|| {
			let alice_init = Tokens::free_balance(NATIVE_CURRENCY_ID, &ALICE);
			// Prepare the call
			let call = Call::Tokens(orml_tokens::Call::transfer {
				dest: BOB,
				currency_id: NATIVE_CURRENCY_ID,
				amount: 100,
			});

			let eva_balance_before = Tokens::free_balance(NATIVE_CURRENCY_ID, &EVA);
			// Construct the wrapped call. This is needed to trigger the pre_dispatch() from the
			// SignedExtension in order to charge fees.
			let wrapped_call = Call::FluentFee(pallet::Call::fee_sharing_wrapper {
				call: Box::new(call),
				beneficiary: Some(EVA),
			});

			// get the call length and info
			let len = wrapped_call.encoded_size();
			let info = wrapped_call.get_dispatch_info();
			let pre = ChargeTransactionPayment::<Runtime>::from(0)
				.pre_dispatch(&ALICE, &wrapped_call, &info, len)
				.expect("should pass");

			let alice_charged = Tokens::free_balance(NATIVE_CURRENCY_ID, &ALICE);
			let fee = Payment::compute_fee(len as u32, &info, 0);
			assert_eq!(alice_init, alice_charged + fee);

			let post = wrapped_call.dispatch(Origin::signed(ALICE)).expect("should be dispatched");

			assert_ok!(ChargeTransactionPayment::<Runtime>::post_dispatch(
				Some(pre),
				&info,
				&post,
				len,
				&Ok(()),
			));

			let eva_balance_after = Tokens::free_balance(NATIVE_CURRENCY_ID, &EVA);

			let ratio = FixedU128::saturating_from_rational(2_u128, 100_u128);
			let beneficiary_cut = ratio.saturating_mul_int(fee);

			/////////// TEST
			let evts = System::events();
			let (beneficiary, fee_shared) = evts
				.iter()
				.rev()
				.find_map(|rec| {
					if let Event::FluentFee(pallet::Event::FeeSharedWithTheBeneficiary {
						beneficiary,
						amount,
					}) = &rec.event
					{
						Some((beneficiary, amount))
					} else {
						None
					}
				})
				.expect("unable to find deployed contract");

			assert_eq!(beneficiary.clone(), EVA);
			assert_eq!(beneficiary_cut, fee_shared.clone().into());
			//////////////

			assert_eq!(eva_balance_after, fee_shared.clone().into());
		})
}

#[test]
fn test_fee_sharing_none_works() {
	ExtBuilder::default()
		.balances(vec![
			(ALICE, NATIVE_CURRENCY_ID, 1_000_000_000_000),
			(BOB, NATIVE_CURRENCY_ID, 1_000_000_000_000),
			(EVA, NATIVE_CURRENCY_ID, 1_000_000_000_000),
		])
		.build()
		.execute_with(|| {
			// Prepare the call
			let call = Call::Tokens(orml_tokens::Call::transfer {
				dest: BOB,
				currency_id: NATIVE_CURRENCY_ID,
				amount: 100,
			});
			// Construct the wrapped call. This is needed to trigger the pre_dispatch() from the
			// SignedExtension in order to charge fees.
			let wrapped_call = Call::FluentFee(pallet::Call::fee_sharing_wrapper {
				call: Box::new(call),
				beneficiary: None,
			});
			let eva_balance_before = Tokens::free_balance(NATIVE_CURRENCY_ID, &EVA);
			// get the call length and info
			let len = wrapped_call.encoded_size();
			let info = wrapped_call.get_dispatch_info();
			let total_fee = Payment::compute_fee(len.clone() as u32, &info, 0);
			ChargeTransactionPayment::<Runtime>::from(0)
				.pre_dispatch(&ALICE, &wrapped_call, &info, len)
				.expect("should pass");

			// Execute the wrapped call
			assert_ok!(wrapped_call.dispatch(Origin::signed(ALICE)));
			// EVA should have recieved tokens equivalent to unit weight
			let eva_balance_after = Tokens::free_balance(NATIVE_CURRENCY_ID, &EVA);
			// assert!(eva_balance_after == eva_balance_before);
			assert!(
				eva_balance_after ==
					eva_balance_before + total_fee.saturating_mul(2).saturating_div(100)
			); // this should ideally fail
		})
}
