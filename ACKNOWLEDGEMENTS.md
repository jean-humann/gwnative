# Acknowledgements

gwnative exists because other Guild Wars community projects established what
was possible. This file records that lineage without implying that those
projects endorse gwnative.

## gw_in_browser

[gw_in_browser](https://github.com/gwdevhub/gw_in_browser), primarily authored
by [Marc Henderkes](https://github.com/henderkes), with contributions from
[Jon](https://github.com/3vcloud), made this project possible. It demonstrated
how ArenaNet's WebAssembly client, patch protocol, streamed image, network
bridges, and browser host contract fit together. gwnative's renderer harness
traces back to that work.

The `.gwmod` manifest and explicit `-modfile` workflow are intentionally
compatible with the format documented by `gw_in_browser`. gwnative implements
its parser and runtime independently in Rust and JavaScript, with host-side
validation appropriate to a native application.

## gwonmac

[gwonmac](https://github.com/Mat4m0/gwonmac), by
[Matthias Amon](https://github.com/Mat4m0), later inspired the direction of a
polished native macOS launcher and companion-tool experience. gwnative is an
independent Rust/AppKit implementation; it does not embed or reproduce
gwonmac's source.

## GWToolbox++ and Daybreak

[GWToolbox++](https://github.com/gwdevhub/GWToolboxpp) and
[Daybreak](https://github.com/gwdevhub/Daybreak) are long-running sources of
ideas about overlays, build libraries, profiles, launch workflows, plugins, and
versioned game integration. Their breadth informs the
[feature compatibility review](docs/feature-compatibility.md).

These projects target different client and operating-system boundaries. A
feature appearing in that review is not evidence that its code was copied or
that it is safe to expose through the current WebAssembly client.

## Ownership and licences

Each upstream project remains governed by its own licence and contributors.
The acknowledgements above grant no rights to Guild Wars material. ArenaNet,
NCSOFT, and game-material ownership are covered by the
[legal notice](NOTICE.md).
