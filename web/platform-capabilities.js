// Module.adProvider and Module.shop — present only so their absence cannot
// crash the client.
//
// ArenaNet's glue guards both namespaces, and both guards are defective. Ad
// playback reports failure and then falls through to use what it just decided
// was missing:
//
//   if (!Module.adProvider || typeof Module.adProvider.showInterstitial !== 'function') {
//     _OnAdFinished(context, 1);
//   }                                    // <- no return
//   _OnAdStarted(context);
//   Module.adProvider.showInterstitial()...
//
// and shop purchase reads through `Module.shop` to reach the property whose
// presence it is checking. Either one is a TypeError that aborts the client, so
// neither namespace can simply be left undefined.
//
// The answer is not to implement them. There is no ad provider and no shop on
// this host, and pretending otherwise would be worse than crashing. Supply the
// exact members each guard reaches for, and have every one of them refuse: the
// client's own error paths then run, which is what it would have done had the
// guards been written correctly. Namespaces whose guards are sound are
// deliberately still absent.

/**
 * @param {(...values: unknown[]) => void} log
 */
export function unavailablePlatformCapabilities(log) {
  /** @param {string} name */
  const unavailable = (name) => {
    log(`[platform] ${name} unavailable`);
    return Promise.reject(new Error(`${name} is unavailable on macOS`));
  };

  return {
    adProvider: Object.freeze({
      privacyOptionsRequired: false,
      showInterstitial: () => unavailable('adProvider.showInterstitial'),
    }),
    shop: {
      // The glue assigns these before calling initialize(). Declared so the
      // contract is visible and the assignments stay ordinary property writes
      // rather than additions to a frozen object.
      onTransactionApproved: null,
      onInAppPurchase: null,
      onTransactionComplete: null,
      onValidationRequest: null,
      initialize: () => unavailable('shop.initialize'),
      inAppPurchase: () => unavailable('shop.inAppPurchase'),
    },
  };
}
