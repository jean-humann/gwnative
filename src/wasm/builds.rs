//! The ArenaNet builds these transforms are certified against, described
//! precisely enough that anything else fails closed.
//!
//! Everything here is data: which stub each bridge replaces, what that stub's
//! body looks like byte-for-byte, and where every call site to it sits. The
//! bodies are recorded rather than merely the indices because an index is a
//! coincidence and a body is a fingerprint — a later build that happens to put
//! a different function at 185 is rejected instead of rewritten.
//!
//! [`ENHANCEMENT_BUILDS`] is the same idea one layer up. Its input is not
//! ArenaNet's module but the output of the template-save transform above, so
//! the two are certified as a chain rather than as alternatives — see
//! [`super::enhancement`].

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum BridgeKind {
    EnsureDirectory,
    FindFiles,
    FileBaseName,
    DeleteFile,
    FileExists,
}

impl BridgeKind {
    /// The dirfd this bridge travels behind. Far outside any fd a real call can
    /// produce, and negative, so the carrier's own callers cannot collide with
    /// it. `web/template-save.js` receives these from the host rather than
    /// restating them, so the side that writes them into the module and the side
    /// that answers them cannot drift apart.
    pub(super) const fn marker(self) -> i64 {
        match self {
            Self::EnsureDirectory => -70_001,
            Self::FindFiles => -70_002,
            Self::FileBaseName => -70_003,
            Self::DeleteFile => -70_004,
            Self::FileExists => -70_005,
        }
    }

    /// The name the page knows this by.
    pub(super) const fn key(self) -> &'static str {
        match self {
            Self::EnsureDirectory => "ensureDirectory",
            Self::FindFiles => "findFiles",
            Self::FileBaseName => "fileBaseName",
            Self::DeleteFile => "deleteFile",
            Self::FileExists => "fileExists",
        }
    }
}

pub(super) const ALL_BRIDGE_KINDS: [BridgeKind; 5] = [
    BridgeKind::EnsureDirectory,
    BridgeKind::FindFiles,
    BridgeKind::FileBaseName,
    BridgeKind::DeleteFile,
    BridgeKind::FileExists,
];

pub(super) struct CallSite {
    /// Index into the code section, i.e. function index minus import count.
    pub local_function: usize,
    /// Byte offset of the `call` opcode inside that function body.
    pub body_offset: usize,
}

pub(super) struct StubBridge {
    pub kind: BridgeKind,
    /// The function the certified call sites currently target. Its type is
    /// reused for the forwarder, and `FileExists` also calls it.
    pub stub_function: usize,
    /// Whole body of the stub, so an unexpected build fails closed. `None` when
    /// the target is a real implementation rather than a stub — there the call
    /// site's own target index is the certification.
    pub stub_body: Option<&'static [u8]>,
    pub call_sites: &'static [CallSite],
}

pub(super) struct KnownBuild {
    pub sha256: &'static str,
    pub output_sha256: &'static str,
    pub import_count: u32,
    /// Import index of `__syscall_newfstatat`, the bridge's carrier.
    pub carrier_import: u32,
    pub bridges: &'static [StubBridge],
}

pub(super) const BUILDS: &[KnownBuild] = &[
    KnownBuild {
        sha256: "b0319704f3072d6948a66026a35af5eb0af12b48d70986783c293e7c77e98483",
        output_sha256: "68c6e09cec0f6992058a44a5617ca9eac7fab4697be1421943bbf664e6d444f6",
        import_count: 219,
        carrier_import: 207,
        bridges: &[
            // PathCreateDirectory(path, recursive) -> error. `i32.const 2` is
            // ERROR_FILE_NOT_FOUND, returned unconditionally.
            StubBridge {
                kind: BridgeKind::EnsureDirectory,
                stub_function: 185,
                stub_body: Some(&[0x00, 0x41, 0x02, 0x0b]),
                call_sites: &[
                    CallSite {
                        local_function: 9538,
                        body_offset: 171,
                    }, // template save
                    CallSite {
                        local_function: 11525,
                        body_offset: 142,
                    }, // chat log
                    CallSite {
                        local_function: 12214,
                        body_offset: 127,
                    }, // screenshot
                ],
            },
            // FindFiles(out, pattern, flags) -> void. An empty body leaves the
            // caller's list zeroed, so every directory reads as empty.
            StubBridge {
                kind: BridgeKind::FindFiles,
                stub_function: 186,
                stub_body: Some(&[0x00, 0x0b]),
                call_sites: &[
                    CallSite {
                        local_function: 9527,
                        body_offset: 157,
                    }, // skills list
                    CallSite {
                        local_function: 9528,
                        body_offset: 157,
                    }, // equipment list
                    CallSite {
                        local_function: 11525,
                        body_offset: 210,
                    }, // chat log
                    CallSite {
                        local_function: 12214,
                        body_offset: 419,
                    }, // screenshot
                ],
            },
            // FileBaseName(dst, _, baseDir, _, path, dstChars) -> written.
            // `i32.const 0` leaves the caller reading uninitialised stack. Only the
            // two template lists are repointed; the model paths that also call it
            // keep the fallback branch they take today.
            StubBridge {
                kind: BridgeKind::FileBaseName,
                stub_function: 197,
                stub_body: Some(&[0x00, 0x41, 0x00, 0x0b]),
                call_sites: &[
                    CallSite {
                        local_function: 9527,
                        body_offset: 276,
                    },
                    CallSite {
                        local_function: 9528,
                        body_offset: 278,
                    },
                ],
            },
            // DeleteFile(path) -> deleted. Not a silent stub like the others: the
            // body is `assert("not implemented")` followed by `unreachable`, so
            // deleting a build aborted the client outright.
            StubBridge {
                kind: BridgeKind::DeleteFile,
                stub_function: 333,
                stub_body: Some(&[
                    0x00, //
                    0x41, 0x9e, 0x87, 0xc5, 0x80, 0x00, //
                    0x41, 0x80, 0xbb, 0xc3, 0x80, 0x00, //
                    0x41, 0xc8, 0x06, //
                    0x10, 0xc2, 0x82, 0x80, 0x80, 0x00, //
                    0x00, //
                    0x0b,
                ]),
                call_sites: &[CallSite {
                    local_function: 459,
                    body_offset: 201,
                }],
            },
            // File::Open(path, mode, err). Mode 1 is meant to open an existing
            // file, and 9757 uses it to ask whether a rename's destination is
            // already taken — but in this build mode 1 opens O_RDWR|O_CREAT, the
            // same as the write mode. The probe creates the file it is testing for,
            // reports it present, and refuses every rename.
            //
            // Only the probe is repointed. The write call five instructions later
            // keeps the real function, and so does the load path.
            StubBridge {
                kind: BridgeKind::FileExists,
                stub_function: 552,
                stub_body: None,
                call_sites: &[CallSite {
                    local_function: 9538,
                    body_offset: 201,
                }],
            },
        ],
    },
    KnownBuild {
        sha256: "3039ca5489eb2bddb38844d275320e3ac070baf01b5b888fc2062982e343f3a8",
        output_sha256: "5a767e11d9f1ae821eca656693f4b4ce5ab16fcf7f9a43c2bf3d094f5e2e5616",
        import_count: 219,
        carrier_import: 207,
        bridges: &[
            StubBridge {
                kind: BridgeKind::EnsureDirectory,
                stub_function: 185,
                stub_body: Some(&[0x00, 0x41, 0x02, 0x0b]),
                call_sites: &[
                    CallSite {
                        local_function: 9541,
                        body_offset: 171,
                    },
                    CallSite {
                        local_function: 11528,
                        body_offset: 142,
                    },
                    CallSite {
                        local_function: 12217,
                        body_offset: 127,
                    },
                ],
            },
            StubBridge {
                kind: BridgeKind::FindFiles,
                stub_function: 186,
                stub_body: Some(&[0x00, 0x0b]),
                call_sites: &[
                    CallSite {
                        local_function: 9530,
                        body_offset: 157,
                    },
                    CallSite {
                        local_function: 9531,
                        body_offset: 157,
                    },
                    CallSite {
                        local_function: 11528,
                        body_offset: 210,
                    },
                    CallSite {
                        local_function: 12217,
                        body_offset: 419,
                    },
                ],
            },
            StubBridge {
                kind: BridgeKind::FileBaseName,
                stub_function: 197,
                stub_body: Some(&[0x00, 0x41, 0x00, 0x0b]),
                call_sites: &[
                    CallSite {
                        local_function: 9530,
                        body_offset: 276,
                    },
                    CallSite {
                        local_function: 9531,
                        body_offset: 278,
                    },
                ],
            },
            StubBridge {
                kind: BridgeKind::DeleteFile,
                stub_function: 333,
                stub_body: Some(&[
                    0x00, //
                    0x41, 0xca, 0x87, 0xc5, 0x80, 0x00, //
                    0x41, 0xa3, 0xbb, 0xc3, 0x80, 0x00, //
                    0x41, 0xc8, 0x06, //
                    0x10, 0xc2, 0x82, 0x80, 0x80, 0x00, //
                    0x00, //
                    0x0b,
                ]),
                call_sites: &[CallSite {
                    local_function: 459,
                    body_offset: 201,
                }],
            },
            StubBridge {
                kind: BridgeKind::FileExists,
                stub_function: 552,
                stub_body: None,
                call_sites: &[CallSite {
                    local_function: 9541,
                    body_offset: 201,
                }],
            },
        ],
    },
    KnownBuild {
        sha256: "cbbc8052014f035a458aa20797fa8150ff4028d1332c0015186fda73c76df14c",
        // Filled from the byte-exact transform after every anchor below was
        // checked against this source hash.
        output_sha256: "706e0873dbc1fe7bdd6837fdb1a09969df133c17812ea0d2336991928380c6e3",
        import_count: 219,
        carrier_import: 207,
        bridges: &[
            StubBridge {
                kind: BridgeKind::EnsureDirectory,
                stub_function: 185,
                stub_body: Some(&[0x00, 0x41, 0x02, 0x0b]),
                call_sites: &[
                    CallSite {
                        local_function: 9541,
                        body_offset: 171,
                    },
                    CallSite {
                        local_function: 11528,
                        body_offset: 142,
                    },
                    CallSite {
                        local_function: 12217,
                        body_offset: 127,
                    },
                ],
            },
            StubBridge {
                kind: BridgeKind::FindFiles,
                stub_function: 186,
                stub_body: Some(&[0x00, 0x0b]),
                call_sites: &[
                    CallSite {
                        local_function: 9530,
                        body_offset: 157,
                    },
                    CallSite {
                        local_function: 9531,
                        body_offset: 157,
                    },
                    CallSite {
                        local_function: 11528,
                        body_offset: 210,
                    },
                    CallSite {
                        local_function: 12217,
                        body_offset: 419,
                    },
                ],
            },
            StubBridge {
                kind: BridgeKind::FileBaseName,
                stub_function: 197,
                stub_body: Some(&[0x00, 0x41, 0x00, 0x0b]),
                call_sites: &[
                    CallSite {
                        local_function: 9530,
                        body_offset: 276,
                    },
                    CallSite {
                        local_function: 9531,
                        body_offset: 278,
                    },
                ],
            },
            StubBridge {
                kind: BridgeKind::DeleteFile,
                stub_function: 333,
                stub_body: Some(&[
                    0x00, //
                    0x41, 0xca, 0x87, 0xc5, 0x80, 0x00, //
                    0x41, 0xa3, 0xbb, 0xc3, 0x80, 0x00, //
                    0x41, 0xc8, 0x06, //
                    0x10, 0xc2, 0x82, 0x80, 0x80, 0x00, //
                    0x00, //
                    0x0b,
                ]),
                call_sites: &[CallSite {
                    local_function: 459,
                    body_offset: 201,
                }],
            },
            StubBridge {
                kind: BridgeKind::FileExists,
                stub_function: 552,
                stub_body: None,
                call_sites: &[CallSite {
                    local_function: 9541,
                    body_offset: 201,
                }],
            },
        ],
    },
];

pub(super) fn find_build(sha256: &str) -> Option<&'static KnownBuild> {
    BUILDS.iter().find(|build| build.sha256 == sha256)
}

/// Where the companion reads the game from, in the client's linear memory.
///
/// Every one of these is an address or an offset that was found by probing a
/// running build, so the whole struct is only meaningful next to the hash it
/// was found in. It is a named struct rather than an array because the names
/// *are* the certification record: `agentX: 0x74` says which field of an agent
/// 0x74 is, and a bare number in a list says nothing anyone could check.
///
/// The order of the fields is the order of the words in the manifest the
/// companion reads, and [`EnhancementLayout::words`] is the one place that
/// order lives. Reordering the struct changes the module's bytes.
pub(super) struct EnhancementLayout {
    pub context_root: u32,
    pub agent_array: u32,
    /// `AvSelectGetTarget` returns the manual target when it is non-zero and
    /// the automatic one otherwise. The companion repeats that exact rule.
    pub manual_target_agent_id: u32,
    pub automatic_target_agent_id: u32,
    pub game_context_slot: u32,
    pub character_context: u32,
    pub map_id: u32,
    pub is_explorable: u32,
    pub current_map_id: u32,
    pub current_instance_type: u32,
    pub player_number: u32,
    pub agent_id: u32,
    pub agent_x: u32,
    pub agent_y: u32,
    pub agent_type: u32,
    pub agent_player_number: u32,
    pub agent_model_type: u32,
    pub game_world_context: u32,
    pub game_party_context: u32,
    pub party_flag: u32,
    pub party_player_party: u32,
    pub party_id: u32,
    pub party_players: u32,
    pub party_henchmen: u32,
    pub party_heroes: u32,
    pub party_others: u32,
    pub party_player_stride: u32,
    pub party_player_login_number: u32,
    pub party_player_called_target_id: u32,
    pub party_player_state: u32,
    pub party_hero_stride: u32,
    pub party_hero_agent_id: u32,
    pub party_hero_owner_player_id: u32,
    pub party_hero_id: u32,
    pub party_hero_level: u32,
    pub party_henchman_stride: u32,
    pub party_henchman_agent_id: u32,
    pub party_henchman_profession: u32,
    pub party_henchman_level: u32,
    pub world_skillbar: u32,
    pub skillbar_stride: u32,
    pub skillbar_agent_id: u32,
    pub skillbar_skills: u32,
    pub skillbar_disabled: u32,
    pub skillbar_cast_count: u32,
    pub skill_stride: u32,
    pub skill_adrenaline_a: u32,
    pub skill_adrenaline_b: u32,
    pub skill_recharge: u32,
    pub skill_id: u32,
    pub skill_event: u32,
    pub world_party_effects: u32,
    pub agent_effects_stride: u32,
    pub agent_effects_agent_id: u32,
    pub agent_effects_buffs: u32,
    pub agent_effects_effects: u32,
    pub buff_stride: u32,
    pub buff_skill_id: u32,
    pub buff_id: u32,
    pub buff_target_agent_id: u32,
    pub effect_stride: u32,
    pub effect_skill_id: u32,
    pub effect_attribute_level: u32,
    pub effect_id: u32,
    pub effect_agent_id: u32,
    pub effect_duration: u32,
    pub effect_timestamp: u32,
    /// The cursor block. The game decodes the active cursor into these fixed
    /// buffers whenever it changes and then calls an Emscripten sink that does
    /// nothing, which is what leaves them readable. `cursor_color_buffer` is
    /// 32×32 BGRA at a pitch of 128, and its own alpha already matches the
    /// redundant A8 mask beside it.
    pub cursor_active_art: u32,
    pub cursor_software_model: u32,
    pub cursor_show_count: u32,
    pub cursor_color_buffer: u32,
    pub cursor_art_hotspot: u32,
    pub cursor_art_texture: u32,
    pub cursor_handle_key: u32,
    pub cursor_handle_object: u32,
    pub cursor_view_texture: u32,
    pub cursor_texture_type: u32,
    pub cursor_texture_width: u32,
    pub cursor_texture_height: u32,
}

impl EnhancementLayout {
    /// The layout as the companion receives it: one word per field, in
    /// declaration order.
    pub(super) fn words(&self) -> [u32; ENHANCEMENT_LAYOUT_WORDS] {
        [
            self.context_root,
            self.agent_array,
            self.manual_target_agent_id,
            self.automatic_target_agent_id,
            self.game_context_slot,
            self.character_context,
            self.map_id,
            self.is_explorable,
            self.current_map_id,
            self.current_instance_type,
            self.player_number,
            self.agent_id,
            self.agent_x,
            self.agent_y,
            self.agent_type,
            self.agent_player_number,
            self.agent_model_type,
            self.game_world_context,
            self.game_party_context,
            self.party_flag,
            self.party_player_party,
            self.party_id,
            self.party_players,
            self.party_henchmen,
            self.party_heroes,
            self.party_others,
            self.party_player_stride,
            self.party_player_login_number,
            self.party_player_called_target_id,
            self.party_player_state,
            self.party_hero_stride,
            self.party_hero_agent_id,
            self.party_hero_owner_player_id,
            self.party_hero_id,
            self.party_hero_level,
            self.party_henchman_stride,
            self.party_henchman_agent_id,
            self.party_henchman_profession,
            self.party_henchman_level,
            self.world_skillbar,
            self.skillbar_stride,
            self.skillbar_agent_id,
            self.skillbar_skills,
            self.skillbar_disabled,
            self.skillbar_cast_count,
            self.skill_stride,
            self.skill_adrenaline_a,
            self.skill_adrenaline_b,
            self.skill_recharge,
            self.skill_id,
            self.skill_event,
            self.world_party_effects,
            self.agent_effects_stride,
            self.agent_effects_agent_id,
            self.agent_effects_buffs,
            self.agent_effects_effects,
            self.buff_stride,
            self.buff_skill_id,
            self.buff_id,
            self.buff_target_agent_id,
            self.effect_stride,
            self.effect_skill_id,
            self.effect_attribute_level,
            self.effect_id,
            self.effect_agent_id,
            self.effect_duration,
            self.effect_timestamp,
            self.cursor_active_art,
            self.cursor_software_model,
            self.cursor_show_count,
            self.cursor_color_buffer,
            self.cursor_art_hotspot,
            self.cursor_art_texture,
            self.cursor_handle_key,
            self.cursor_handle_object,
            self.cursor_view_texture,
            self.cursor_texture_type,
            self.cursor_texture_width,
            self.cursor_texture_height,
        ]
    }
}

/// Fields in [`EnhancementLayout`]. Named because the companion's own `Layout`
/// is this many words long and the two have to agree.
pub(super) const ENHANCEMENT_LAYOUT_WORDS: usize = 79;

pub(super) struct EnhancementBuild {
    /// The *template-save* output, not ArenaNet's own module. That transform is
    /// the floor every launch lands on, so layering this on top of it is what
    /// keeps opting in from costing template save.
    pub sha256: &'static str,
    pub output_sha256: &'static str,
    /// Imported functions, which is where the local index space starts. The
    /// template-save transform only appends, so this is the same count as
    /// [`KnownBuild::import_count`] — asserted below rather than assumed.
    pub import_count: u32,
    pub program_id: u32,
    pub build_id: u32,
    /// ArenaNet's exported browser-driven client loop,
    /// `EmscriptenExeThreadMainLoop`. The older GWCA `FrApi`/`LeaveGameThread`
    /// anchor runs only during startup on this build and is no use as a tick.
    pub hook_function: u32,
    /// The signature that function must have, as value-type bytes. `0x7f` is
    /// `i32`; the dispatcher is generated from the parameter count, so a build
    /// whose loop took two arguments would still be refused here rather than
    /// rewritten into something that calls it wrong.
    pub hook_params: &'static [u8],
    pub hook_results: &'static [u8],
    /// The table slot the dispatcher borrows to reach the companion. Emscripten
    /// reserves slot 0 for the null function pointer and never fills it.
    pub table_slot: u32,
    pub layout: EnhancementLayout,
}

pub(super) const ENHANCEMENT_BUILDS: &[EnhancementBuild] = &[
    EnhancementBuild {
        sha256: "68c6e09cec0f6992058a44a5617ca9eac7fab4697be1421943bbf664e6d444f6",
        output_sha256: "64d37404c8937c3efaf42d102a266bbba77b1d905e3f7e34cc39d6ff97f31306",
        import_count: 219,
        program_id: 1,
        build_id: 38771,
        hook_function: 446,
        hook_params: &[0x7f],
        hook_results: &[],
        table_slot: 0,
        layout: EnhancementLayout {
            context_root: 0x5a_0e20,
            agent_array: 0x5a_4d98,
            manual_target_agent_id: 0x5a_388c,
            automatic_target_agent_id: 0x5a_3888,
            game_context_slot: 6,
            character_context: 0x44,
            map_id: 0x198,
            is_explorable: 0x19c,
            current_map_id: 0x234,
            current_instance_type: 0x23c,
            player_number: 0x2ac,
            agent_id: 0x2c,
            agent_x: 0x74,
            agent_y: 0x78,
            agent_type: 0x9c,
            agent_player_number: 0xf4,
            agent_model_type: 0xf6,
            game_world_context: 0x2c,
            game_party_context: 0x4c,
            party_flag: 0x14,
            party_player_party: 0x54,
            party_id: 0x00,
            party_players: 0x04,
            party_henchmen: 0x14,
            party_heroes: 0x24,
            party_others: 0x34,
            party_player_stride: 0x0c,
            party_player_login_number: 0x00,
            party_player_called_target_id: 0x04,
            party_player_state: 0x08,
            party_hero_stride: 0x18,
            party_hero_agent_id: 0x00,
            party_hero_owner_player_id: 0x04,
            party_hero_id: 0x08,
            party_hero_level: 0x14,
            party_henchman_stride: 0x34,
            party_henchman_agent_id: 0x00,
            party_henchman_profession: 0x2c,
            party_henchman_level: 0x30,
            world_skillbar: 0x6f0,
            skillbar_stride: 0xbc,
            skillbar_agent_id: 0x00,
            skillbar_skills: 0x04,
            skillbar_disabled: 0xa4,
            skillbar_cast_count: 0xb0,
            skill_stride: 0x14,
            skill_adrenaline_a: 0x00,
            skill_adrenaline_b: 0x04,
            skill_recharge: 0x08,
            skill_id: 0x0c,
            skill_event: 0x10,
            world_party_effects: 0x508,
            agent_effects_stride: 0x24,
            agent_effects_agent_id: 0x00,
            agent_effects_buffs: 0x04,
            agent_effects_effects: 0x14,
            buff_stride: 0x10,
            buff_skill_id: 0x00,
            buff_id: 0x08,
            buff_target_agent_id: 0x0c,
            effect_stride: 0x18,
            effect_skill_id: 0x00,
            effect_attribute_level: 0x04,
            effect_id: 0x08,
            effect_agent_id: 0x0c,
            effect_duration: 0x10,
            effect_timestamp: 0x14,
            cursor_active_art: 0x5a_1620,
            cursor_software_model: 0x5a_1624,
            cursor_show_count: 0x5a_1628,
            cursor_color_buffer: 0x29_8d90,
            cursor_art_hotspot: 0x00,
            cursor_art_texture: 0x0c,
            cursor_handle_key: 0x08,
            cursor_handle_object: 0x00,
            cursor_view_texture: 0x08,
            cursor_texture_type: 0x0c,
            cursor_texture_width: 0x14,
            cursor_texture_height: 0x18,
        },
    },
    EnhancementBuild {
        sha256: "5a767e11d9f1ae821eca656693f4b4ce5ab16fcf7f9a43c2bf3d094f5e2e5616",
        output_sha256: "4138857647cb3d74879e186f9c34b0be7363154f435bef29020a834d438dfc18",
        import_count: 219,
        program_id: 1,
        build_id: 38790,
        hook_function: 446,
        hook_params: &[0x7f],
        hook_results: &[],
        table_slot: 0,
        layout: EnhancementLayout {
            context_root: 0x5a_0f10,
            agent_array: 0x5a_4e88,
            manual_target_agent_id: 0x5a_397c,
            automatic_target_agent_id: 0x5a_3978,
            game_context_slot: 6,
            character_context: 0x44,
            map_id: 0x198,
            is_explorable: 0x19c,
            current_map_id: 0x234,
            current_instance_type: 0x23c,
            player_number: 0x2ac,
            agent_id: 0x2c,
            agent_x: 0x74,
            agent_y: 0x78,
            agent_type: 0x9c,
            agent_player_number: 0xf4,
            agent_model_type: 0xf6,
            game_world_context: 0x2c,
            game_party_context: 0x4c,
            party_flag: 0x14,
            party_player_party: 0x54,
            party_id: 0x00,
            party_players: 0x04,
            party_henchmen: 0x14,
            party_heroes: 0x24,
            party_others: 0x34,
            party_player_stride: 0x0c,
            party_player_login_number: 0x00,
            party_player_called_target_id: 0x04,
            party_player_state: 0x08,
            party_hero_stride: 0x18,
            party_hero_agent_id: 0x00,
            party_hero_owner_player_id: 0x04,
            party_hero_id: 0x08,
            party_hero_level: 0x14,
            party_henchman_stride: 0x34,
            party_henchman_agent_id: 0x00,
            party_henchman_profession: 0x2c,
            party_henchman_level: 0x30,
            world_skillbar: 0x6f0,
            skillbar_stride: 0xbc,
            skillbar_agent_id: 0x00,
            skillbar_skills: 0x04,
            skillbar_disabled: 0xa4,
            skillbar_cast_count: 0xb0,
            skill_stride: 0x14,
            skill_adrenaline_a: 0x00,
            skill_adrenaline_b: 0x04,
            skill_recharge: 0x08,
            skill_id: 0x0c,
            skill_event: 0x10,
            world_party_effects: 0x508,
            agent_effects_stride: 0x24,
            agent_effects_agent_id: 0x00,
            agent_effects_buffs: 0x04,
            agent_effects_effects: 0x14,
            buff_stride: 0x10,
            buff_skill_id: 0x00,
            buff_id: 0x08,
            buff_target_agent_id: 0x0c,
            effect_stride: 0x18,
            effect_skill_id: 0x00,
            effect_attribute_level: 0x04,
            effect_id: 0x08,
            effect_agent_id: 0x0c,
            effect_duration: 0x10,
            effect_timestamp: 0x14,
            cursor_active_art: 0x5a_1710,
            cursor_software_model: 0x5a_1714,
            cursor_show_count: 0x5a_1718,
            cursor_color_buffer: 0x29_8e80,
            cursor_art_hotspot: 0x00,
            cursor_art_texture: 0x0c,
            cursor_handle_key: 0x08,
            cursor_handle_object: 0x00,
            cursor_view_texture: 0x08,
            cursor_texture_type: 0x0c,
            cursor_texture_width: 0x14,
            cursor_texture_height: 0x18,
        },
    },
    EnhancementBuild {
        sha256: "706e0873dbc1fe7bdd6837fdb1a09969df133c17812ea0d2336991928380c6e3",
        output_sha256: "e4cacf1c17addd1296c23dcf4114556b4104855544046dafcae66d4b7c62d10a",
        import_count: 219,
        program_id: 1,
        build_id: 38795,
        hook_function: 446,
        hook_params: &[0x7f],
        hook_results: &[],
        table_slot: 0,
        layout: EnhancementLayout {
            context_root: 0x5a_0ee0,
            agent_array: 0x5a_4e58,
            manual_target_agent_id: 0x5a_394c,
            automatic_target_agent_id: 0x5a_3948,
            game_context_slot: 6,
            character_context: 0x44,
            map_id: 0x198,
            is_explorable: 0x19c,
            current_map_id: 0x234,
            current_instance_type: 0x23c,
            player_number: 0x2ac,
            agent_id: 0x2c,
            agent_x: 0x74,
            agent_y: 0x78,
            agent_type: 0x9c,
            agent_player_number: 0xf4,
            agent_model_type: 0xf6,
            game_world_context: 0x2c,
            game_party_context: 0x4c,
            party_flag: 0x14,
            party_player_party: 0x54,
            party_id: 0x00,
            party_players: 0x04,
            party_henchmen: 0x14,
            party_heroes: 0x24,
            party_others: 0x34,
            party_player_stride: 0x0c,
            party_player_login_number: 0x00,
            party_player_called_target_id: 0x04,
            party_player_state: 0x08,
            party_hero_stride: 0x18,
            party_hero_agent_id: 0x00,
            party_hero_owner_player_id: 0x04,
            party_hero_id: 0x08,
            party_hero_level: 0x14,
            party_henchman_stride: 0x34,
            party_henchman_agent_id: 0x00,
            party_henchman_profession: 0x2c,
            party_henchman_level: 0x30,
            world_skillbar: 0x6f0,
            skillbar_stride: 0xbc,
            skillbar_agent_id: 0x00,
            skillbar_skills: 0x04,
            skillbar_disabled: 0xa4,
            skillbar_cast_count: 0xb0,
            skill_stride: 0x14,
            skill_adrenaline_a: 0x00,
            skill_adrenaline_b: 0x04,
            skill_recharge: 0x08,
            skill_id: 0x0c,
            skill_event: 0x10,
            world_party_effects: 0x508,
            agent_effects_stride: 0x24,
            agent_effects_agent_id: 0x00,
            agent_effects_buffs: 0x04,
            agent_effects_effects: 0x14,
            buff_stride: 0x10,
            buff_skill_id: 0x00,
            buff_id: 0x08,
            buff_target_agent_id: 0x0c,
            effect_stride: 0x18,
            effect_skill_id: 0x00,
            effect_attribute_level: 0x04,
            effect_id: 0x08,
            effect_agent_id: 0x0c,
            effect_duration: 0x10,
            effect_timestamp: 0x14,
            cursor_active_art: 0x5a_16e0,
            cursor_software_model: 0x5a_16e4,
            cursor_show_count: 0x5a_16e8,
            cursor_color_buffer: 0x29_8e50,
            cursor_art_hotspot: 0x00,
            cursor_art_texture: 0x0c,
            cursor_handle_key: 0x08,
            cursor_handle_object: 0x00,
            cursor_view_texture: 0x08,
            cursor_texture_type: 0x0c,
            cursor_texture_width: 0x14,
            cursor_texture_height: 0x18,
        },
    },
];

pub(super) fn find_enhancement_build(sha256: &str) -> Option<&'static EnhancementBuild> {
    ENHANCEMENT_BUILDS
        .iter()
        .find(|build| build.sha256 == sha256)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_marker_is_distinct_and_out_of_reach_of_a_real_fd() {
        let mut seen = std::collections::HashSet::new();
        for kind in ALL_BRIDGE_KINDS {
            assert!(
                seen.insert(kind.marker()),
                "duplicate marker for {}",
                kind.key()
            );
            // A real dirfd is a small non-negative integer or AT_FDCWD (-100).
            assert!(kind.marker() < -1000);
        }
    }

    #[test]
    fn the_table_is_self_consistent() {
        for build in BUILDS {
            assert_eq!(build.sha256.len(), 64);
            assert_eq!(build.output_sha256.len(), 64);
            assert!(build.carrier_import < build.import_count);
            let kinds: Vec<_> = build.bridges.iter().map(|b| b.kind).collect();
            for kind in ALL_BRIDGE_KINDS {
                assert!(kinds.contains(&kind), "{} has no bridge", kind.key());
            }
            for bridge in build.bridges {
                assert!(
                    !bridge.call_sites.is_empty(),
                    "{} repoints nothing",
                    bridge.kind.key()
                );
            }
        }
    }

    /// The two tables are one chain, and nothing else says so.
    ///
    /// The enhancement transform reads a module the template-save transform
    /// wrote, and takes its import count on trust because appending functions
    /// cannot change one. If a later template-save entry ever did change it,
    /// every enhancement index — the hook function most of all — would be off
    /// by that difference and the rewrite would still produce a module, just
    /// one that dispatches the wrong function.
    #[test]
    fn the_enhancement_table_is_layered_on_the_template_save_one() {
        for build in ENHANCEMENT_BUILDS {
            assert_eq!(build.sha256.len(), 64);
            assert_eq!(build.output_sha256.len(), 64);
            let base = BUILDS
                .iter()
                .find(|template| template.output_sha256 == build.sha256)
                .expect("an enhancement input no template-save transform produces");
            assert_eq!(
                build.import_count, base.import_count,
                "appending forwarders cannot change the import count",
            );
            assert!(
                build.hook_function >= build.import_count,
                "hook is imported"
            );
            assert_eq!(build.layout.words().len(), ENHANCEMENT_LAYOUT_WORDS);
        }
        assert!(find_enhancement_build("not a hash").is_none());
        assert!(find_enhancement_build(ENHANCEMENT_BUILDS[0].sha256).is_some());
    }
}
