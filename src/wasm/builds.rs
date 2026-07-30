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
    KnownBuild {
        sha256: "3229678d3fd7d2f0e309530086a614d97f02e7eeb3ca12650ababfd2eb360817",
        // Build 38797 changes only functions 477, 5009, and 17491 from 38795.
        // The bridge stubs and all thirteen call-site bodies are byte-identical
        // at the same indices, so their independently checked anchors remain
        // exact rather than being inferred from a nearby build.
        output_sha256: "9ee332604a9b2adbdfa1a8ab217f4fd1dac58b01a2443e037bc5bd11f279d094",
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
/// Every one of these is an address or offset tied to the exact module hash
/// beside it and cross-checked against the pinned client layouts and bounded
/// runtime fixtures. It is a named struct rather than an array because the
/// names *are* the certification record: `agentX: 0x74` says which field of an
/// agent 0x74 is, and a bare number in a list says nothing anyone could check.
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
    pub agent_z: u32,
    pub agent_rotation: u32,
    pub agent_type: u32,
    pub agent_player_number: u32,
    pub agent_model_type: u32,
    pub agent_primary: u32,
    pub agent_secondary: u32,
    pub agent_level: u32,
    pub agent_hp: u32,
    pub agent_model_state: u32,
    pub agent_effects: u32,
    pub agent_allegiance: u32,
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
    pub world_active_quest: u32,
    pub world_quest_log: u32,
    pub quest_stride: u32,
    pub quest_id: u32,
    pub quest_log_state: u32,
    pub quest_map_from: u32,
    pub quest_marker: u32,
    pub quest_map_to: u32,
    pub world_mission_objectives: u32,
    pub mission_objective_stride: u32,
    pub mission_objective_id: u32,
    pub mission_objective_type: u32,
    pub world_missions_completed: u32,
    pub world_missions_bonus: u32,
    pub world_missions_completed_hm: u32,
    pub world_missions_bonus_hm: u32,
    pub world_unlocked_map: u32,
    pub world_vanquished_areas: u32,
    pub game_item_context: u32,
    pub item_context_inventory: u32,
    pub inventory_bags: u32,
    pub inventory_storage_panes: u32,
    pub inventory_gold_character: u32,
    pub inventory_gold_storage: u32,
    pub bag_type: u32,
    pub bag_index: u32,
    pub bag_container_item: u32,
    pub bag_items_count: u32,
    pub bag_items: u32,
    pub item_id: u32,
    pub item_agent_id: u32,
    pub item_bag: u32,
    pub item_modifiers: u32,
    pub item_modifier_count: u32,
    pub item_customized: u32,
    pub item_model_file_id: u32,
    pub item_type: u32,
    pub item_dye: u32,
    pub item_value: u32,
    pub item_interaction: u32,
    pub item_model_id: u32,
    pub item_formula: u32,
    pub item_material_salvageable: u32,
    pub item_quantity: u32,
    pub item_equipped: u32,
    pub item_profession: u32,
    pub item_slot: u32,
    /// Direct `FriendList` object. Its string and UUID fields are deliberately
    /// absent from the public snapshot; only bounded numeric status is read.
    pub friend_list_address: u32,
    pub friend_list_friends: u32,
    pub friend_list_number_friend: u32,
    pub friend_list_number_ignore: u32,
    pub friend_list_number_partner: u32,
    pub friend_list_number_trade: u32,
    pub friend_list_player_status: u32,
    pub friend_type: u32,
    pub friend_status: u32,
    pub friend_id: u32,
    pub friend_zone_id: u32,
    pub game_guild_context: u32,
    pub guild_context_player_index: u32,
    pub guild_context_player_key: u32,
    pub guild_context_player_rank: u32,
    pub guild_context_guilds: u32,
    pub guild_context_roster: u32,
    pub guild_key: u32,
    pub guild_index: u32,
    pub guild_rank: u32,
    pub guild_features: u32,
    pub guild_rating: u32,
    pub guild_faction: u32,
    pub guild_faction_point: u32,
    pub guild_qualifier_point: u32,
    pub guild_cape: u32,
    pub guild_player_stride: u32,
    pub guild_player_name_pointer: u32,
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
    /// Direct `Camera` singleton selected from the exact `GmCam.cpp` routines
    /// in this build. Only the stable read-only GWCA fields below cross the
    /// companion boundary; transition targets and controller pointers do not.
    pub camera_address: u32,
    pub camera_look_at_agent_id: u32,
    pub camera_max_distance: u32,
    pub camera_yaw: u32,
    pub camera_pitch: u32,
    pub camera_distance: u32,
    pub camera_position: u32,
    pub camera_look_at_target: u32,
    pub camera_field_of_view: u32,
    pub camera_mode: u32,
    /// `GameContext::trade` is the same +0x58 pointer in GWCA, Py4GW Native,
    /// and the compiled accessor (function 3975) in every certified client.
    /// The remaining values describe the fixed 0x38 `TradeContext`; only
    /// bounded offer summaries cross the companion boundary.
    pub game_trade_context: u32,
    pub trade_flags: u32,
    pub trade_player_gold: u32,
    pub trade_player_items: u32,
    pub trade_partner_gold: u32,
    pub trade_partner_items: u32,
    pub trade_item_stride: u32,
    pub trade_item_id: u32,
    pub trade_item_quantity: u32,
    /// Direct `GWArray<Frame*>` descriptor selected from the compiled
    /// `GetFrameById` accessor in this exact client. GWCA and Py4GW agree on
    /// the scalar identity, state, and position fields below. Callback,
    /// label, relation-list, and message fields deliberately stay closed.
    pub ui_frame_array: u32,
    pub ui_frame_size: u32,
    pub ui_frame_visibility_flags: u32,
    pub ui_frame_type: u32,
    pub ui_frame_template_type: u32,
    pub ui_frame_child_offset_id: u32,
    pub ui_frame_id: u32,
    pub ui_frame_position: u32,
    pub ui_position_flags: u32,
    pub ui_position_left: u32,
    pub ui_position_bottom: u32,
    pub ui_position_right: u32,
    pub ui_position_top: u32,
    pub ui_frame_parent_relation: u32,
    pub ui_frame_hash: u32,
    pub ui_frame_state: u32,
    // Append-only ABI: keep every previously certified layout word stable.
    pub world_merchant_items: u32,
    pub world_hard_mode_unlocked: u32,
    pub world_experience: u32,
    pub world_experience_duplicate: u32,
    pub world_kurzick_current: u32,
    pub world_kurzick_current_duplicate: u32,
    pub world_kurzick_total: u32,
    pub world_kurzick_total_duplicate: u32,
    pub world_kurzick_maximum: u32,
    pub world_luxon_current: u32,
    pub world_luxon_current_duplicate: u32,
    pub world_luxon_total: u32,
    pub world_luxon_total_duplicate: u32,
    pub world_luxon_maximum: u32,
    pub world_imperial_current: u32,
    pub world_imperial_current_duplicate: u32,
    pub world_imperial_total: u32,
    pub world_imperial_total_duplicate: u32,
    pub world_imperial_maximum: u32,
    pub world_level: u32,
    pub world_level_duplicate: u32,
    pub world_balthazar_current: u32,
    pub world_balthazar_current_duplicate: u32,
    pub world_balthazar_total: u32,
    pub world_balthazar_total_duplicate: u32,
    pub world_balthazar_maximum: u32,
    pub world_skill_points_current: u32,
    pub world_skill_points_current_duplicate: u32,
    pub world_skill_points_total: u32,
    pub world_skill_points_total_duplicate: u32,
    pub game_account_context: u32,
    pub account_unlocked_skills: u32,
    pub world_learnable_character_skills: u32,
    pub world_unlocked_character_skills: u32,
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
            self.agent_z,
            self.agent_rotation,
            self.agent_type,
            self.agent_player_number,
            self.agent_model_type,
            self.agent_primary,
            self.agent_secondary,
            self.agent_level,
            self.agent_hp,
            self.agent_model_state,
            self.agent_effects,
            self.agent_allegiance,
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
            self.world_active_quest,
            self.world_quest_log,
            self.quest_stride,
            self.quest_id,
            self.quest_log_state,
            self.quest_map_from,
            self.quest_marker,
            self.quest_map_to,
            self.world_mission_objectives,
            self.mission_objective_stride,
            self.mission_objective_id,
            self.mission_objective_type,
            self.world_missions_completed,
            self.world_missions_bonus,
            self.world_missions_completed_hm,
            self.world_missions_bonus_hm,
            self.world_unlocked_map,
            self.world_vanquished_areas,
            self.game_item_context,
            self.item_context_inventory,
            self.inventory_bags,
            self.inventory_storage_panes,
            self.inventory_gold_character,
            self.inventory_gold_storage,
            self.bag_type,
            self.bag_index,
            self.bag_container_item,
            self.bag_items_count,
            self.bag_items,
            self.item_id,
            self.item_agent_id,
            self.item_bag,
            self.item_modifiers,
            self.item_modifier_count,
            self.item_customized,
            self.item_model_file_id,
            self.item_type,
            self.item_dye,
            self.item_value,
            self.item_interaction,
            self.item_model_id,
            self.item_formula,
            self.item_material_salvageable,
            self.item_quantity,
            self.item_equipped,
            self.item_profession,
            self.item_slot,
            self.friend_list_address,
            self.friend_list_friends,
            self.friend_list_number_friend,
            self.friend_list_number_ignore,
            self.friend_list_number_partner,
            self.friend_list_number_trade,
            self.friend_list_player_status,
            self.friend_type,
            self.friend_status,
            self.friend_id,
            self.friend_zone_id,
            self.game_guild_context,
            self.guild_context_player_index,
            self.guild_context_player_key,
            self.guild_context_player_rank,
            self.guild_context_guilds,
            self.guild_context_roster,
            self.guild_key,
            self.guild_index,
            self.guild_rank,
            self.guild_features,
            self.guild_rating,
            self.guild_faction,
            self.guild_faction_point,
            self.guild_qualifier_point,
            self.guild_cape,
            self.guild_player_stride,
            self.guild_player_name_pointer,
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
            self.camera_address,
            self.camera_look_at_agent_id,
            self.camera_max_distance,
            self.camera_yaw,
            self.camera_pitch,
            self.camera_distance,
            self.camera_position,
            self.camera_look_at_target,
            self.camera_field_of_view,
            self.camera_mode,
            self.game_trade_context,
            self.trade_flags,
            self.trade_player_gold,
            self.trade_player_items,
            self.trade_partner_gold,
            self.trade_partner_items,
            self.trade_item_stride,
            self.trade_item_id,
            self.trade_item_quantity,
            self.ui_frame_array,
            self.ui_frame_size,
            self.ui_frame_visibility_flags,
            self.ui_frame_type,
            self.ui_frame_template_type,
            self.ui_frame_child_offset_id,
            self.ui_frame_id,
            self.ui_frame_position,
            self.ui_position_flags,
            self.ui_position_left,
            self.ui_position_bottom,
            self.ui_position_right,
            self.ui_position_top,
            self.ui_frame_parent_relation,
            self.ui_frame_hash,
            self.ui_frame_state,
            self.world_merchant_items,
            self.world_hard_mode_unlocked,
            self.world_experience,
            self.world_experience_duplicate,
            self.world_kurzick_current,
            self.world_kurzick_current_duplicate,
            self.world_kurzick_total,
            self.world_kurzick_total_duplicate,
            self.world_kurzick_maximum,
            self.world_luxon_current,
            self.world_luxon_current_duplicate,
            self.world_luxon_total,
            self.world_luxon_total_duplicate,
            self.world_luxon_maximum,
            self.world_imperial_current,
            self.world_imperial_current_duplicate,
            self.world_imperial_total,
            self.world_imperial_total_duplicate,
            self.world_imperial_maximum,
            self.world_level,
            self.world_level_duplicate,
            self.world_balthazar_current,
            self.world_balthazar_current_duplicate,
            self.world_balthazar_total,
            self.world_balthazar_total_duplicate,
            self.world_balthazar_maximum,
            self.world_skill_points_current,
            self.world_skill_points_current_duplicate,
            self.world_skill_points_total,
            self.world_skill_points_total_duplicate,
            self.game_account_context,
            self.account_unlocked_skills,
            self.world_learnable_character_skills,
            self.world_unlocked_character_skills,
        ]
    }
}

/// Fields in [`EnhancementLayout`]. Named because the companion's own `Layout`
/// is this many words long and the two have to agree.
pub(super) const ENHANCEMENT_LAYOUT_WORDS: usize = 232;

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
    /// The high-level UI message gateway. Dialog body/button wrappers in each
    /// certified module call this exact `(message_id, wparam, lparam)` entry.
    /// The enhancement transform keeps the original call first and adds a
    /// passive companion observer afterward; it never replaces UI handling.
    pub ui_message_function: u32,
    /// The one table slot both transformed gateways borrow to reach the
    /// companion's typed dispatcher. Emscripten reserves slot 0 for the null
    /// function pointer and never fills it.
    pub table_slot: u32,
    /// The existing `(i32, i32, i32, i32) -> ()` type used by the shared
    /// dispatcher. The first word distinguishes a game tick from an observed
    /// UI message, so the module needs no second empty table slot.
    pub dispatch_type: u32,
    /// Returns the exact memory layout certified for this build. A function
    /// pointer lets a data-identical client revision deliberately reuse the
    /// preceding layout without copying 232 words and inviting transcription
    /// drift; the registry tests still exercise every resolved build.
    pub layout: fn() -> &'static EnhancementLayout,
}

pub(super) const ENHANCEMENT_BUILDS: &[EnhancementBuild] = &[
    EnhancementBuild {
        sha256: "68c6e09cec0f6992058a44a5617ca9eac7fab4697be1421943bbf664e6d444f6",
        output_sha256: "6ca299c82688ac265a692e7bc3f6188a22ff5eaba8aabc45114d53da0861f69e",
        import_count: 219,
        program_id: 1,
        build_id: 38771,
        hook_function: 446,
        hook_params: &[0x7f],
        hook_results: &[],
        ui_message_function: 6839,
        table_slot: 0,
        dispatch_type: 14,
        layout: || &EnhancementLayout {
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
            agent_z: 0x30,
            agent_rotation: 0x4c,
            agent_type: 0x9c,
            agent_player_number: 0xf4,
            agent_model_type: 0xf6,
            agent_primary: 0x10e,
            agent_secondary: 0x10f,
            agent_level: 0x110,
            agent_hp: 0x134,
            agent_model_state: 0x158,
            agent_effects: 0x13c,
            agent_allegiance: 0x1b5,
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
            world_active_quest: 0x528,
            world_quest_log: 0x52c,
            quest_stride: 0x34,
            quest_id: 0x00,
            quest_log_state: 0x04,
            quest_map_from: 0x14,
            quest_marker: 0x18,
            quest_map_to: 0x28,
            world_mission_objectives: 0x564,
            mission_objective_stride: 0x0c,
            mission_objective_id: 0x00,
            mission_objective_type: 0x08,
            world_missions_completed: 0x5cc,
            world_missions_bonus: 0x5dc,
            world_missions_completed_hm: 0x5ec,
            world_missions_bonus_hm: 0x5fc,
            world_unlocked_map: 0x60c,
            world_vanquished_areas: 0x83c,
            game_item_context: 0x40,
            item_context_inventory: 0xf8,
            inventory_bags: 0x00,
            inventory_storage_panes: 0x60,
            inventory_gold_character: 0x90,
            inventory_gold_storage: 0x94,
            bag_type: 0x00,
            bag_index: 0x04,
            bag_container_item: 0x0c,
            bag_items_count: 0x10,
            bag_items: 0x18,
            item_id: 0x00,
            item_agent_id: 0x04,
            item_bag: 0x0c,
            item_modifiers: 0x10,
            item_modifier_count: 0x14,
            item_customized: 0x18,
            item_model_file_id: 0x1c,
            item_type: 0x20,
            item_dye: 0x21,
            item_value: 0x24,
            item_interaction: 0x28,
            item_model_id: 0x2c,
            item_formula: 0x48,
            item_material_salvageable: 0x4a,
            item_quantity: 0x4c,
            item_equipped: 0x4e,
            item_profession: 0x4f,
            item_slot: 0x50,
            friend_list_address: 0x5a_4f88,
            friend_list_friends: 0x00,
            friend_list_number_friend: 0x24,
            friend_list_number_ignore: 0x28,
            friend_list_number_partner: 0x2c,
            friend_list_number_trade: 0x30,
            friend_list_player_status: 0xa0,
            friend_type: 0x00,
            friend_status: 0x04,
            friend_id: 0x40,
            friend_zone_id: 0x44,
            game_guild_context: 0x3c,
            guild_context_player_index: 0x60,
            guild_context_player_key: 0x64,
            guild_context_player_rank: 0x2a0,
            guild_context_guilds: 0x2f8,
            guild_context_roster: 0x358,
            guild_key: 0x00,
            guild_index: 0x24,
            guild_rank: 0x28,
            guild_features: 0x2c,
            guild_rating: 0x70,
            guild_faction: 0x74,
            guild_faction_point: 0x78,
            guild_qualifier_point: 0x7c,
            guild_cape: 0x90,
            guild_player_stride: 0x174,
            guild_player_name_pointer: 0x04,
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
            camera_address: 0x5a_b904,
            camera_look_at_agent_id: 0x00,
            camera_max_distance: 0x10,
            camera_yaw: 0x18,
            camera_pitch: 0x1c,
            camera_distance: 0x20,
            camera_position: 0x78,
            camera_look_at_target: 0xa8,
            camera_field_of_view: 0xc0,
            camera_mode: 0x11c,
            game_trade_context: 0x58,
            trade_flags: 0x00,
            trade_player_gold: 0x10,
            trade_player_items: 0x14,
            trade_partner_gold: 0x24,
            trade_partner_items: 0x28,
            trade_item_stride: 0x08,
            trade_item_id: 0x00,
            trade_item_quantity: 0x04,
            ui_frame_array: 0x5a_1f8c,
            ui_frame_size: 0x1c8,
            ui_frame_visibility_flags: 0x18,
            ui_frame_type: 0x20,
            ui_frame_template_type: 0x24,
            ui_frame_child_offset_id: 0xb8,
            ui_frame_id: 0xbc,
            ui_frame_position: 0xd8,
            ui_position_flags: 0x00,
            ui_position_left: 0x04,
            ui_position_bottom: 0x08,
            ui_position_right: 0x0c,
            ui_position_top: 0x10,
            ui_frame_parent_relation: 0x128,
            ui_frame_hash: 0x134,
            ui_frame_state: 0x18c,
            world_merchant_items: 0x24,
            world_hard_mode_unlocked: 0x684,
            world_experience: 0x740,
            world_experience_duplicate: 0x744,
            world_kurzick_current: 0x748,
            world_kurzick_current_duplicate: 0x74c,
            world_kurzick_total: 0x750,
            world_kurzick_total_duplicate: 0x754,
            world_kurzick_maximum: 0x7b8,
            world_luxon_current: 0x758,
            world_luxon_current_duplicate: 0x75c,
            world_luxon_total: 0x760,
            world_luxon_total_duplicate: 0x764,
            world_luxon_maximum: 0x7bc,
            world_imperial_current: 0x768,
            world_imperial_current_duplicate: 0x76c,
            world_imperial_total: 0x770,
            world_imperial_total_duplicate: 0x774,
            world_imperial_maximum: 0x7c4,
            world_level: 0x788,
            world_level_duplicate: 0x78c,
            world_balthazar_current: 0x798,
            world_balthazar_current_duplicate: 0x79c,
            world_balthazar_total: 0x7a0,
            world_balthazar_total_duplicate: 0x7a4,
            world_balthazar_maximum: 0x7c0,
            world_skill_points_current: 0x7a8,
            world_skill_points_current_duplicate: 0x7ac,
            world_skill_points_total: 0x7b0,
            world_skill_points_total_duplicate: 0x7b4,
            game_account_context: 0x28,
            account_unlocked_skills: 0x124,
            world_learnable_character_skills: 0x700,
            world_unlocked_character_skills: 0x710,
        },
    },
    EnhancementBuild {
        sha256: "5a767e11d9f1ae821eca656693f4b4ce5ab16fcf7f9a43c2bf3d094f5e2e5616",
        output_sha256: "396f01af69f68295c521ec76a71e360b11afaf6f1b59435b88dc4007de9ee972",
        import_count: 219,
        program_id: 1,
        build_id: 38790,
        hook_function: 446,
        hook_params: &[0x7f],
        hook_results: &[],
        ui_message_function: 6842,
        table_slot: 0,
        dispatch_type: 14,
        layout: || &EnhancementLayout {
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
            agent_z: 0x30,
            agent_rotation: 0x4c,
            agent_type: 0x9c,
            agent_player_number: 0xf4,
            agent_model_type: 0xf6,
            agent_primary: 0x10e,
            agent_secondary: 0x10f,
            agent_level: 0x110,
            agent_hp: 0x134,
            agent_model_state: 0x158,
            agent_effects: 0x13c,
            agent_allegiance: 0x1b5,
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
            world_active_quest: 0x528,
            world_quest_log: 0x52c,
            quest_stride: 0x34,
            quest_id: 0x00,
            quest_log_state: 0x04,
            quest_map_from: 0x14,
            quest_marker: 0x18,
            quest_map_to: 0x28,
            world_mission_objectives: 0x564,
            mission_objective_stride: 0x0c,
            mission_objective_id: 0x00,
            mission_objective_type: 0x08,
            world_missions_completed: 0x5cc,
            world_missions_bonus: 0x5dc,
            world_missions_completed_hm: 0x5ec,
            world_missions_bonus_hm: 0x5fc,
            world_unlocked_map: 0x60c,
            world_vanquished_areas: 0x83c,
            game_item_context: 0x40,
            item_context_inventory: 0xf8,
            inventory_bags: 0x00,
            inventory_storage_panes: 0x60,
            inventory_gold_character: 0x90,
            inventory_gold_storage: 0x94,
            bag_type: 0x00,
            bag_index: 0x04,
            bag_container_item: 0x0c,
            bag_items_count: 0x10,
            bag_items: 0x18,
            item_id: 0x00,
            item_agent_id: 0x04,
            item_bag: 0x0c,
            item_modifiers: 0x10,
            item_modifier_count: 0x14,
            item_customized: 0x18,
            item_model_file_id: 0x1c,
            item_type: 0x20,
            item_dye: 0x21,
            item_value: 0x24,
            item_interaction: 0x28,
            item_model_id: 0x2c,
            item_formula: 0x48,
            item_material_salvageable: 0x4a,
            item_quantity: 0x4c,
            item_equipped: 0x4e,
            item_profession: 0x4f,
            item_slot: 0x50,
            friend_list_address: 0x5a_5078,
            friend_list_friends: 0x00,
            friend_list_number_friend: 0x24,
            friend_list_number_ignore: 0x28,
            friend_list_number_partner: 0x2c,
            friend_list_number_trade: 0x30,
            friend_list_player_status: 0xa0,
            friend_type: 0x00,
            friend_status: 0x04,
            friend_id: 0x40,
            friend_zone_id: 0x44,
            game_guild_context: 0x3c,
            guild_context_player_index: 0x60,
            guild_context_player_key: 0x64,
            guild_context_player_rank: 0x2a0,
            guild_context_guilds: 0x2f8,
            guild_context_roster: 0x358,
            guild_key: 0x00,
            guild_index: 0x24,
            guild_rank: 0x28,
            guild_features: 0x2c,
            guild_rating: 0x70,
            guild_faction: 0x74,
            guild_faction_point: 0x78,
            guild_qualifier_point: 0x7c,
            guild_cape: 0x90,
            guild_player_stride: 0x174,
            guild_player_name_pointer: 0x04,
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
            camera_address: 0x5a_b9f4,
            camera_look_at_agent_id: 0x00,
            camera_max_distance: 0x10,
            camera_yaw: 0x18,
            camera_pitch: 0x1c,
            camera_distance: 0x20,
            camera_position: 0x78,
            camera_look_at_target: 0xa8,
            camera_field_of_view: 0xc0,
            camera_mode: 0x11c,
            game_trade_context: 0x58,
            trade_flags: 0x00,
            trade_player_gold: 0x10,
            trade_player_items: 0x14,
            trade_partner_gold: 0x24,
            trade_partner_items: 0x28,
            trade_item_stride: 0x08,
            trade_item_id: 0x00,
            trade_item_quantity: 0x04,
            ui_frame_array: 0x5a_207c,
            ui_frame_size: 0x1c8,
            ui_frame_visibility_flags: 0x18,
            ui_frame_type: 0x20,
            ui_frame_template_type: 0x24,
            ui_frame_child_offset_id: 0xb8,
            ui_frame_id: 0xbc,
            ui_frame_position: 0xd8,
            ui_position_flags: 0x00,
            ui_position_left: 0x04,
            ui_position_bottom: 0x08,
            ui_position_right: 0x0c,
            ui_position_top: 0x10,
            ui_frame_parent_relation: 0x128,
            ui_frame_hash: 0x134,
            ui_frame_state: 0x18c,
            world_merchant_items: 0x24,
            world_hard_mode_unlocked: 0x684,
            world_experience: 0x740,
            world_experience_duplicate: 0x744,
            world_kurzick_current: 0x748,
            world_kurzick_current_duplicate: 0x74c,
            world_kurzick_total: 0x750,
            world_kurzick_total_duplicate: 0x754,
            world_kurzick_maximum: 0x7b8,
            world_luxon_current: 0x758,
            world_luxon_current_duplicate: 0x75c,
            world_luxon_total: 0x760,
            world_luxon_total_duplicate: 0x764,
            world_luxon_maximum: 0x7bc,
            world_imperial_current: 0x768,
            world_imperial_current_duplicate: 0x76c,
            world_imperial_total: 0x770,
            world_imperial_total_duplicate: 0x774,
            world_imperial_maximum: 0x7c4,
            world_level: 0x788,
            world_level_duplicate: 0x78c,
            world_balthazar_current: 0x798,
            world_balthazar_current_duplicate: 0x79c,
            world_balthazar_total: 0x7a0,
            world_balthazar_total_duplicate: 0x7a4,
            world_balthazar_maximum: 0x7c0,
            world_skill_points_current: 0x7a8,
            world_skill_points_current_duplicate: 0x7ac,
            world_skill_points_total: 0x7b0,
            world_skill_points_total_duplicate: 0x7b4,
            game_account_context: 0x28,
            account_unlocked_skills: 0x124,
            world_learnable_character_skills: 0x700,
            world_unlocked_character_skills: 0x710,
        },
    },
    EnhancementBuild {
        sha256: "706e0873dbc1fe7bdd6837fdb1a09969df133c17812ea0d2336991928380c6e3",
        output_sha256: "5a4d82c5d1f2435f0cb9c99611071ab355d87d3f6f911eae61aaef3cabdcfc9e",
        import_count: 219,
        program_id: 1,
        build_id: 38795,
        hook_function: 446,
        hook_params: &[0x7f],
        hook_results: &[],
        ui_message_function: 6842,
        table_slot: 0,
        dispatch_type: 14,
        layout: || &EnhancementLayout {
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
            agent_z: 0x30,
            agent_rotation: 0x4c,
            agent_type: 0x9c,
            agent_player_number: 0xf4,
            agent_model_type: 0xf6,
            agent_primary: 0x10e,
            agent_secondary: 0x10f,
            agent_level: 0x110,
            agent_hp: 0x134,
            agent_model_state: 0x158,
            agent_effects: 0x13c,
            agent_allegiance: 0x1b5,
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
            world_active_quest: 0x528,
            world_quest_log: 0x52c,
            quest_stride: 0x34,
            quest_id: 0x00,
            quest_log_state: 0x04,
            quest_map_from: 0x14,
            quest_marker: 0x18,
            quest_map_to: 0x28,
            world_mission_objectives: 0x564,
            mission_objective_stride: 0x0c,
            mission_objective_id: 0x00,
            mission_objective_type: 0x08,
            world_missions_completed: 0x5cc,
            world_missions_bonus: 0x5dc,
            world_missions_completed_hm: 0x5ec,
            world_missions_bonus_hm: 0x5fc,
            world_unlocked_map: 0x60c,
            world_vanquished_areas: 0x83c,
            game_item_context: 0x40,
            item_context_inventory: 0xf8,
            inventory_bags: 0x00,
            inventory_storage_panes: 0x60,
            inventory_gold_character: 0x90,
            inventory_gold_storage: 0x94,
            bag_type: 0x00,
            bag_index: 0x04,
            bag_container_item: 0x0c,
            bag_items_count: 0x10,
            bag_items: 0x18,
            item_id: 0x00,
            item_agent_id: 0x04,
            item_bag: 0x0c,
            item_modifiers: 0x10,
            item_modifier_count: 0x14,
            item_customized: 0x18,
            item_model_file_id: 0x1c,
            item_type: 0x20,
            item_dye: 0x21,
            item_value: 0x24,
            item_interaction: 0x28,
            item_model_id: 0x2c,
            item_formula: 0x48,
            item_material_salvageable: 0x4a,
            item_quantity: 0x4c,
            item_equipped: 0x4e,
            item_profession: 0x4f,
            item_slot: 0x50,
            friend_list_address: 0x5a_5048,
            friend_list_friends: 0x00,
            friend_list_number_friend: 0x24,
            friend_list_number_ignore: 0x28,
            friend_list_number_partner: 0x2c,
            friend_list_number_trade: 0x30,
            friend_list_player_status: 0xa0,
            friend_type: 0x00,
            friend_status: 0x04,
            friend_id: 0x40,
            friend_zone_id: 0x44,
            game_guild_context: 0x3c,
            guild_context_player_index: 0x60,
            guild_context_player_key: 0x64,
            guild_context_player_rank: 0x2a0,
            guild_context_guilds: 0x2f8,
            guild_context_roster: 0x358,
            guild_key: 0x00,
            guild_index: 0x24,
            guild_rank: 0x28,
            guild_features: 0x2c,
            guild_rating: 0x70,
            guild_faction: 0x74,
            guild_faction_point: 0x78,
            guild_qualifier_point: 0x7c,
            guild_cape: 0x90,
            guild_player_stride: 0x174,
            guild_player_name_pointer: 0x04,
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
            camera_address: 0x5a_b9c4,
            camera_look_at_agent_id: 0x00,
            camera_max_distance: 0x10,
            camera_yaw: 0x18,
            camera_pitch: 0x1c,
            camera_distance: 0x20,
            camera_position: 0x78,
            camera_look_at_target: 0xa8,
            camera_field_of_view: 0xc0,
            camera_mode: 0x11c,
            game_trade_context: 0x58,
            trade_flags: 0x00,
            trade_player_gold: 0x10,
            trade_player_items: 0x14,
            trade_partner_gold: 0x24,
            trade_partner_items: 0x28,
            trade_item_stride: 0x08,
            trade_item_id: 0x00,
            trade_item_quantity: 0x04,
            ui_frame_array: 0x5a_204c,
            ui_frame_size: 0x1c8,
            ui_frame_visibility_flags: 0x18,
            ui_frame_type: 0x20,
            ui_frame_template_type: 0x24,
            ui_frame_child_offset_id: 0xb8,
            ui_frame_id: 0xbc,
            ui_frame_position: 0xd8,
            ui_position_flags: 0x00,
            ui_position_left: 0x04,
            ui_position_bottom: 0x08,
            ui_position_right: 0x0c,
            ui_position_top: 0x10,
            ui_frame_parent_relation: 0x128,
            ui_frame_hash: 0x134,
            ui_frame_state: 0x18c,
            world_merchant_items: 0x24,
            world_hard_mode_unlocked: 0x684,
            world_experience: 0x740,
            world_experience_duplicate: 0x744,
            world_kurzick_current: 0x748,
            world_kurzick_current_duplicate: 0x74c,
            world_kurzick_total: 0x750,
            world_kurzick_total_duplicate: 0x754,
            world_kurzick_maximum: 0x7b8,
            world_luxon_current: 0x758,
            world_luxon_current_duplicate: 0x75c,
            world_luxon_total: 0x760,
            world_luxon_total_duplicate: 0x764,
            world_luxon_maximum: 0x7bc,
            world_imperial_current: 0x768,
            world_imperial_current_duplicate: 0x76c,
            world_imperial_total: 0x770,
            world_imperial_total_duplicate: 0x774,
            world_imperial_maximum: 0x7c4,
            world_level: 0x788,
            world_level_duplicate: 0x78c,
            world_balthazar_current: 0x798,
            world_balthazar_current_duplicate: 0x79c,
            world_balthazar_total: 0x7a0,
            world_balthazar_total_duplicate: 0x7a4,
            world_balthazar_maximum: 0x7c0,
            world_skill_points_current: 0x7a8,
            world_skill_points_current_duplicate: 0x7ac,
            world_skill_points_total: 0x7b0,
            world_skill_points_total_duplicate: 0x7b4,
            game_account_context: 0x28,
            account_unlocked_skills: 0x124,
            world_learnable_character_skills: 0x700,
            world_unlocked_character_skills: 0x710,
        },
    },
    EnhancementBuild {
        sha256: "9ee332604a9b2adbdfa1a8ab217f4fd1dac58b01a2443e037bc5bd11f279d094",
        output_sha256: "6fb84c0c2b1ffaaca34e9545df06145ed13cd1ee17110ccb873bec3610cb59d8",
        import_count: 219,
        program_id: 1,
        build_id: 38797,
        hook_function: 446,
        hook_params: &[0x7f],
        hook_results: &[],
        ui_message_function: 6842,
        table_slot: 0,
        dispatch_type: 14,
        // The source data section is byte-identical to 38795, and the only
        // changed functions (477, 5009, and 17491) are outside every context,
        // global, call-site, stub, and main-loop anchor used by the companion.
        layout: || (ENHANCEMENT_BUILDS[2].layout)(),
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
            assert_eq!((build.layout)().words().len(), ENHANCEMENT_LAYOUT_WORDS);
        }
        assert!(find_enhancement_build("not a hash").is_none());
        assert!(find_enhancement_build(ENHANCEMENT_BUILDS[0].sha256).is_some());
    }

    #[test]
    fn every_certified_build_uses_the_verified_completion_layout() {
        for build in ENHANCEMENT_BUILDS {
            let layout = (build.layout)();
            assert_eq!(layout.world_missions_completed, 0x5cc);
            assert_eq!(layout.world_missions_bonus, 0x5dc);
            assert_eq!(layout.world_missions_completed_hm, 0x5ec);
            assert_eq!(layout.world_missions_bonus_hm, 0x5fc);
            assert_eq!(layout.world_unlocked_map, 0x60c);
            assert_eq!(layout.world_vanquished_areas, 0x83c);
        }
    }

    #[test]
    fn every_certified_build_uses_the_verified_camera_layout() {
        let addresses = [0x5a_b904, 0x5a_b9f4, 0x5a_b9c4, 0x5a_b9c4];
        for (build, address) in ENHANCEMENT_BUILDS.iter().zip(addresses) {
            let layout = (build.layout)();
            assert_eq!(layout.camera_address, address);
            assert_eq!(layout.camera_look_at_agent_id, 0x00);
            assert_eq!(layout.camera_max_distance, 0x10);
            assert_eq!(layout.camera_yaw, 0x18);
            assert_eq!(layout.camera_pitch, 0x1c);
            assert_eq!(layout.camera_distance, 0x20);
            assert_eq!(layout.camera_position, 0x78);
            assert_eq!(layout.camera_look_at_target, 0xa8);
            assert_eq!(layout.camera_field_of_view, 0xc0);
            assert_eq!(layout.camera_mode, 0x11c);
        }
    }

    #[test]
    fn every_certified_build_uses_the_verified_trade_layout() {
        for build in ENHANCEMENT_BUILDS {
            let layout = (build.layout)();
            assert_eq!(layout.game_trade_context, 0x58);
            assert_eq!(layout.trade_flags, 0x00);
            assert_eq!(layout.trade_player_gold, 0x10);
            assert_eq!(layout.trade_player_items, 0x14);
            assert_eq!(layout.trade_partner_gold, 0x24);
            assert_eq!(layout.trade_partner_items, 0x28);
            assert_eq!(layout.trade_item_stride, 0x08);
            assert_eq!(layout.trade_item_id, 0x00);
            assert_eq!(layout.trade_item_quantity, 0x04);
        }
    }

    #[test]
    fn every_certified_build_uses_the_verified_ui_frame_layout() {
        let arrays = [0x5a_1f8c, 0x5a_207c, 0x5a_204c, 0x5a_204c];
        for (build, array) in ENHANCEMENT_BUILDS.iter().zip(arrays) {
            let layout = (build.layout)();
            assert_eq!(layout.ui_frame_array, array);
            assert_eq!(layout.ui_frame_size, 0x1c8);
            assert_eq!(layout.ui_frame_visibility_flags, 0x18);
            assert_eq!(layout.ui_frame_type, 0x20);
            assert_eq!(layout.ui_frame_template_type, 0x24);
            assert_eq!(layout.ui_frame_child_offset_id, 0xb8);
            assert_eq!(layout.ui_frame_id, 0xbc);
            assert_eq!(layout.ui_frame_position, 0xd8);
            assert_eq!(layout.ui_position_flags, 0x00);
            assert_eq!(layout.ui_position_left, 0x04);
            assert_eq!(layout.ui_position_bottom, 0x08);
            assert_eq!(layout.ui_position_right, 0x0c);
            assert_eq!(layout.ui_position_top, 0x10);
            assert_eq!(layout.ui_frame_parent_relation, 0x128);
            assert_eq!(layout.ui_frame_hash, 0x134);
            assert_eq!(layout.ui_frame_state, 0x18c);
        }
    }

    #[test]
    fn every_certified_build_uses_the_verified_merchant_layout() {
        for build in ENHANCEMENT_BUILDS {
            let layout = (build.layout)();
            assert_eq!(layout.game_world_context, 0x2c);
            assert_eq!(layout.world_merchant_items, 0x24);
        }
    }

    #[test]
    fn every_certified_build_uses_the_verified_progression_layout() {
        for build in ENHANCEMENT_BUILDS {
            let layout = (build.layout)();
            assert_eq!(layout.game_world_context, 0x2c);
            assert_eq!(layout.world_hard_mode_unlocked, 0x684);
            assert_eq!(layout.world_experience, 0x740);
            assert_eq!(layout.world_experience_duplicate, 0x744);
            assert_eq!(layout.world_kurzick_current, 0x748);
            assert_eq!(layout.world_kurzick_current_duplicate, 0x74c);
            assert_eq!(layout.world_kurzick_total, 0x750);
            assert_eq!(layout.world_kurzick_total_duplicate, 0x754);
            assert_eq!(layout.world_luxon_current, 0x758);
            assert_eq!(layout.world_luxon_current_duplicate, 0x75c);
            assert_eq!(layout.world_luxon_total, 0x760);
            assert_eq!(layout.world_luxon_total_duplicate, 0x764);
            assert_eq!(layout.world_imperial_current, 0x768);
            assert_eq!(layout.world_imperial_current_duplicate, 0x76c);
            assert_eq!(layout.world_imperial_total, 0x770);
            assert_eq!(layout.world_imperial_total_duplicate, 0x774);
            assert_eq!(layout.world_level, 0x788);
            assert_eq!(layout.world_level_duplicate, 0x78c);
            assert_eq!(layout.world_balthazar_current, 0x798);
            assert_eq!(layout.world_balthazar_current_duplicate, 0x79c);
            assert_eq!(layout.world_balthazar_total, 0x7a0);
            assert_eq!(layout.world_balthazar_total_duplicate, 0x7a4);
            assert_eq!(layout.world_skill_points_current, 0x7a8);
            assert_eq!(layout.world_skill_points_current_duplicate, 0x7ac);
            assert_eq!(layout.world_skill_points_total, 0x7b0);
            assert_eq!(layout.world_skill_points_total_duplicate, 0x7b4);
            assert_eq!(layout.world_kurzick_maximum, 0x7b8);
            assert_eq!(layout.world_luxon_maximum, 0x7bc);
            assert_eq!(layout.world_balthazar_maximum, 0x7c0);
            assert_eq!(layout.world_imperial_maximum, 0x7c4);
        }
    }

    #[test]
    fn every_certified_build_uses_the_verified_skill_unlock_layout() {
        for build in ENHANCEMENT_BUILDS {
            let layout = (build.layout)();
            assert_eq!(layout.game_world_context, 0x2c);
            assert_eq!(layout.game_account_context, 0x28);
            assert_eq!(layout.account_unlocked_skills, 0x124);
            assert_eq!(layout.world_learnable_character_skills, 0x700);
            assert_eq!(layout.world_unlocked_character_skills, 0x710);
        }
    }
}
