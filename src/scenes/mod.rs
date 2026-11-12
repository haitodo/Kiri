//! Scene management, differential scene switching, window binding resolution, and layout snapshots.

pub mod bindings;
pub mod layouts;
pub mod manager;
pub mod switch;

#[cfg(test)]
mod tests {
    use super::bindings::BindingResolver;
    use super::manager::{SceneState, SceneStateManager};
    use super::switch::{execute_scene_switch, verify_switch_results, SwitchWarning};
    use crate::config::{AppConfig, MonitorPolicy, RectData, Scene, TitleRule, WindowBinding, WindowMatch};
    use crate::platform::fake::FakeWindowSystem;
    use crate::platform::hooks::{MinimizeEvent, MinimizeEventKind};
    use crate::platform::{ShowCmd, WindowInstance, WindowSystem};
    use crate::recovery::RecoveryManager;
    use std::path::PathBuf;

    fn test_journal_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "kiri_journal_{}_{}_{}.json",
            std::process::id(),
            crate::diagnostics::current_unix_ms(),
            name
        ));
        p
    }

    fn make_win(
        hwnd: u64,
        pid: u32,
        exe: &str,
        class: &str,
        title: &str,
        monitor_id: &str,
    ) -> WindowInstance {
        WindowInstance {
            hwnd,
            pid,
            process_start_time: 1000 + pid as u64,
            exe_path: exe.to_string(),
            window_class: class.to_string(),
            title: title.to_string(),
            aumid: None,
            monitor_id: monitor_id.to_string(),
            is_visible: true,
            is_iconic: false,
            is_enabled: true,
            is_hung: false,
            is_maximized: false,
            has_taskbar_button: true,
            is_elevated: false,
            normal_rect: RectData {
                left: 2000,
                top: 100,
                right: 3000,
                bottom: 800,
            },
            window_rect: RectData {
                left: 2000,
                top: 100,
                right: 3000,
                bottom: 800,
            },
            z_order: 0,
        }
    }

    fn setup_standard_config() -> AppConfig {
        let mut cfg = AppConfig::default_initial();
        cfg.target.monitor_policy = MonitorPolicy::Fixed;
        cfg.target.monitor_id = Some("MON-2-TARGET".to_string());
        cfg.bindings = vec![
            WindowBinding {
                id: "mt5".to_string(),
                match_rule: WindowMatch {
                    exe_path: r"C:\Apps\MT5\terminal64.exe".to_string(),
                    window_class: "MetaQuotes::MetaTrader::5.00".to_string(),
                    title_rule: TitleRule::any(),
                    aumid: None,
                },
                instance_hint: None,
                scene_ids: vec!["trade".to_string()],
                always_visible: false,
                enabled: true,
            },
            WindowBinding {
                id: "vscode".to_string(),
                match_rule: WindowMatch {
                    exe_path: r"C:\Apps\VSCode\Code.exe".to_string(),
                    window_class: "Chrome_WidgetWin_1".to_string(),
                    title_rule: TitleRule::contains("Visual Studio Code"),
                    aumid: None,
                },
                instance_hint: None,
                scene_ids: vec!["code".to_string()],
                always_visible: false,
                enabled: true,
            },
            WindowBinding {
                id: "shared-calc".to_string(),
                match_rule: WindowMatch {
                    exe_path: r"C:\Apps\Shared\shared.exe".to_string(),
                    window_class: "SharedClass".to_string(),
                    title_rule: TitleRule::any(),
                    aumid: None,
                },
                instance_hint: None,
                scene_ids: vec!["trade".to_string(), "code".to_string()],
                always_visible: false,
                enabled: true,
            },
        ];
        cfg
    }

    #[test]
    fn test_at21_unknown_initial_state_aligns_target_monitor_and_preserves_other_monitor() {
        let cfg = setup_standard_config();
        let mut sys = FakeWindowSystem::new_dual_monitor();
        // Monitor 1: Edge (unregistered, on MON-1-PRIMARY)
        sys.add_window(make_win(
            10,
            100,
            r"C:\Program Files\Microsoft\Edge\msedge.exe",
            "Chrome_WidgetWin_1",
            "Market News - Edge",
            "MON-1-PRIMARY",
        ));
        // Monitor 2: MT5 (trade), VSCode (code), Shared (trade+code), Notepad (unregistered)
        sys.add_window(make_win(
            20,
            200,
            r"C:\Apps\MT5\terminal64.exe",
            "MetaQuotes::MetaTrader::5.00",
            "12345678: USDJPY,M1",
            "MON-2-TARGET",
        ));
        sys.add_window(make_win(
            30,
            300,
            r"C:\Apps\VSCode\Code.exe",
            "Chrome_WidgetWin_1",
            "main.rs - Visual Studio Code",
            "MON-2-TARGET",
        ));
        sys.add_window(make_win(
            40,
            400,
            r"C:\Apps\Shared\shared.exe",
            "SharedClass",
            "Shared Tool",
            "MON-2-TARGET",
        ));
        sys.add_window(make_win(
            50,
            500,
            r"C:\Windows\notepad.exe",
            "Notepad",
            "notes.txt",
            "MON-2-TARGET",
        ));

        let mut state_mgr = SceneStateManager::new(Some("trade".to_string()));
        assert_eq!(*state_mgr.state(), SceneState::Unknown);

        let mut resolver = BindingResolver::new();
        let jpath = test_journal_path("at21");
        let mut recovery = RecoveryManager::new(jpath.clone());

        // Click "code" while in Unknown state -> MT5 (20) minimized; VSCode (30), Shared (40), Notepad (50), Edge (10) untouched
        let out = execute_scene_switch(
            "code",
            &cfg,
            &mut state_mgr,
            &mut resolver,
            &mut recovery,
            &mut sys,
            Some("MON-1-PRIMARY"), // Even if launcher is on Monitor 1 (AT-22)!
        );
        assert!(out.switched);
        assert_eq!(out.minimized_hwnds, vec![20]);
        assert!(out.restored_hwnds.is_empty());
        assert_eq!(*state_mgr.state(), SceneState::Active("code".to_string()));

        // Verify Monitor 1 Edge and Monitor 2 unregistered Notepad are not iconic
        assert!(!sys.query_window(10).unwrap().is_iconic);
        assert!(sys.query_window(20).unwrap().is_iconic);
        assert!(!sys.query_window(30).unwrap().is_iconic);
        assert!(!sys.query_window(40).unwrap().is_iconic);
        assert!(!sys.query_window(50).unwrap().is_iconic);

        // AT-03: Clicking "code" again is an idempotent no-op
        let out2 = execute_scene_switch(
            "code",
            &cfg,
            &mut state_mgr,
            &mut resolver,
            &mut recovery,
            &mut sys,
            Some("MON-1-PRIMARY"),
        );
        assert!(out2.was_noop_same_scene);
        assert!(!out2.switched);

        // Switch back to "trade" -> VSCode (30) minimized, MT5 (20) restored, Shared (40) untouched
        let out3 = execute_scene_switch(
            "trade",
            &cfg,
            &mut state_mgr,
            &mut resolver,
            &mut recovery,
            &mut sys,
            Some("MON-1-PRIMARY"),
        );
        assert!(out3.switched);
        assert_eq!(out3.minimized_hwnds, vec![30]);
        assert_eq!(out3.restored_hwnds, vec![20]);
        assert!(!sys.query_window(20).unwrap().is_iconic);
        assert!(sys.query_window(30).unwrap().is_iconic);

        let _ = std::fs::remove_file(jpath);
    }

    #[test]
    fn test_at09_and_at23_user_manual_minimize_and_manual_restore_reminimize_never_auto_restored() {
        let cfg = setup_standard_config();
        let mut sys = FakeWindowSystem::new_dual_monitor();
        sys.add_window(make_win(
            20,
            200,
            r"C:\Apps\MT5\terminal64.exe",
            "MetaQuotes::MetaTrader::5.00",
            "MT5",
            "MON-2-TARGET",
        ));
        sys.add_window(make_win(
            30,
            300,
            r"C:\Apps\VSCode\Code.exe",
            "Chrome_WidgetWin_1",
            "Visual Studio Code",
            "MON-2-TARGET",
        ));

        let mut state_mgr = SceneStateManager::new(None);
        let mut resolver = BindingResolver::new();
        let jpath = test_journal_path("at23");
        let mut recovery = RecoveryManager::new(jpath.clone());

        // AT-09: User manually minimizes MT5 (20) BEFORE switching to "code"
        sys.simulate_user_minimize(20);
        let _ = execute_scene_switch(
            "code",
            &cfg,
            &mut state_mgr,
            &mut resolver,
            &mut recovery,
            &mut sys,
            None,
        );
        // Switch to "trade" -> MT5 was minimized by user, NOT Kiri, so it must NOT be restored!
        let out = execute_scene_switch(
            "trade",
            &cfg,
            &mut state_mgr,
            &mut resolver,
            &mut recovery,
            &mut sys,
            None,
        );
        assert!(out.restored_hwnds.is_empty());
        assert!(sys.query_window(20).unwrap().is_iconic);

        // Now user restores MT5 (20), then Kiri switches to "code" (so Kiri minimizes 20)
        sys.simulate_user_restore(20);
        let out_code = execute_scene_switch(
            "code",
            &cfg,
            &mut state_mgr,
            &mut resolver,
            &mut recovery,
            &mut sys,
            None,
        );
        assert_eq!(out_code.minimized_hwnds, vec![20]);

        // AT-23: User manually restores MT5 (20) from taskbar (fires MINIMIZEEND), then manually re-minimizes it
        sys.simulate_user_restore(20);
        recovery.process_minimize_events(&[MinimizeEvent {
            hwnd: 20,
            kind: MinimizeEventKind::MinimizeEnd,
            timestamp_ms: 5000,
        }]);
        sys.simulate_user_minimize(20);
        recovery.process_minimize_events(&[MinimizeEvent {
            hwnd: 20,
            kind: MinimizeEventKind::MinimizeStart,
            timestamp_ms: 6000,
        }]);

        // Switch back to "trade" -> Because user manually restored & re-minimized 20, ManagedAction was invalidated!
        let out_trade = execute_scene_switch(
            "trade",
            &cfg,
            &mut state_mgr,
            &mut resolver,
            &mut recovery,
            &mut sys,
            None,
        );
        assert!(out_trade.restored_hwnds.is_empty());
        assert!(sys.query_window(20).unwrap().is_iconic);

        let _ = std::fs::remove_file(jpath);
    }

    #[test]
    fn test_at25_foreground_window_minimized_last_with_sw_minimize_and_at26_modal_skipped() {
        let cfg = setup_standard_config();
        let mut sys = FakeWindowSystem::new_dual_monitor();
        let mut mt5 = make_win(
            20,
            200,
            r"C:\Apps\MT5\terminal64.exe",
            "MetaQuotes::MetaTrader::5.00",
            "MT5",
            "MON-2-TARGET",
        );
        // AT-26: Suppose MT5 has a modal order dialog open -> `is_enabled = false`
        mt5.is_enabled = false;
        sys.add_window(mt5);
        sys.add_window(make_win(
            30,
            300,
            r"C:\Apps\VSCode\Code.exe",
            "Chrome_WidgetWin_1",
            "Visual Studio Code",
            "MON-2-TARGET",
        ));

        let mut state_mgr = SceneStateManager::new(None);
        let mut resolver = BindingResolver::new();
        let jpath = test_journal_path("at25_26");
        let mut recovery = RecoveryManager::new(jpath.clone());

        // Switching to "code" while MT5 is disabled (modal open) -> skips MT5 and warns!
        let out = execute_scene_switch(
            "code",
            &cfg,
            &mut state_mgr,
            &mut resolver,
            &mut recovery,
            &mut sys,
            None,
        );
        assert!(out.minimized_hwnds.is_empty());
        assert!(out.warnings.contains(&SwitchWarning::SkippedModalDisabledWindow {
            binding_id: "mt5".to_string(),
            hwnd: 20,
        }));
        assert!(!sys.query_window(20).unwrap().is_iconic);

        // Now close modal dialog (`is_enabled = true`) and make VSCode (30) the foreground window
        sys.windows.iter_mut().find(|w| w.hwnd == 20).unwrap().is_enabled = true;
        // Minimize MT5 via switch to "code" first
        state_mgr.reset_to_unknown();
        let _ = execute_scene_switch(
            "code",
            &cfg,
            &mut state_mgr,
            &mut resolver,
            &mut recovery,
            &mut sys,
            None,
        );
        assert!(sys.query_window(20).unwrap().is_iconic);

        // User is typing in VSCode (foreground_hwnd = Some(30)), then clicks "trade"
        sys.foreground_hwnd = Some(30);
        sys.issued_commands.clear();

        let out_trade = execute_scene_switch(
            "trade",
            &cfg,
            &mut state_mgr,
            &mut resolver,
            &mut recovery,
            &mut sys,
            None,
        );
        assert_eq!(out_trade.restored_hwnds, vec![20]);
        assert_eq!(out_trade.minimized_hwnds, vec![30]);

        // Verify order of OS commands (Section 5.4, FR-18, AT-25):
        // 1. Restore MT5 (20) with RestoreNoActivate FIRST
        // 2. Minimize foreground VSCode (30) with MinimizeActivateNext (SW_MINIMIZE) SECOND
        assert_eq!(sys.issued_commands.len(), 2);
        assert_eq!(sys.issued_commands[0].hwnd, 20);
        assert_eq!(sys.issued_commands[0].cmd, ShowCmd::RestoreNoActivate);
        assert_eq!(sys.issued_commands[1].hwnd, 30);
        assert_eq!(sys.issued_commands[1].cmd, ShowCmd::MinimizeActivateNext);
        // And foreground focus moved off the minimized VSCode (30) onto MT5 (20)!
        assert_eq!(sys.get_foreground_window(), Some(20));

        let _ = std::fs::remove_file(jpath);
    }

    #[test]
    fn test_tray_hiding_app_detected_during_verification() {
        let cfg = setup_standard_config();
        let mut sys = FakeWindowSystem::new_dual_monitor();
        sys.add_window(make_win(
            20,
            200,
            r"C:\Apps\MT5\terminal64.exe",
            "MetaQuotes::MetaTrader::5.00",
            "MT5",
            "MON-2-TARGET",
        ));
        // Configure HWND 20 to hide itself to tray (`!is_visible && !is_iconic`) on minimize
        sys.tray_hiding_hwnds.insert(20);

        let mut state_mgr = SceneStateManager::new(None);
        let mut resolver = BindingResolver::new();
        let jpath = test_journal_path("tray_hide");
        let mut recovery = RecoveryManager::new(jpath.clone());

        let out = execute_scene_switch(
            "code",
            &cfg,
            &mut state_mgr,
            &mut resolver,
            &mut recovery,
            &mut sys,
            None,
        );
        let verif = out.verification.expect("should schedule verification");
        let v_warnings = verify_switch_results(&verif, &mut recovery, &mut sys);
        assert!(v_warnings.contains(&SwitchWarning::HiddenToTrayOnMinimize {
            binding_id: "mt5".to_string(),
            hwnd: 20,
        }));

        let _ = std::fs::remove_file(jpath);
    }

    #[test]
    fn test_at10_ambiguous_same_monitor_and_cross_monitor_confirmation_rules() {
        let mut cfg = setup_standard_config();
        // Add an Edge binding without instance_hint
        cfg.bindings.push(WindowBinding {
            id: "web-edge".to_string(),
            match_rule: WindowMatch {
                exe_path: r"C:\Program Files\Microsoft\Edge\msedge.exe".to_string(),
                window_class: "Chrome_WidgetWin_1".to_string(),
                title_rule: TitleRule::any(),
                aumid: None,
            },
            instance_hint: None,
            scene_ids: vec!["web".to_string()],
            always_visible: false,
            enabled: true,
        });

        let mut sys = FakeWindowSystem::new_dual_monitor();
        // Two identical MT5 windows on MON-2-TARGET -> AmbiguousSameMonitor (AT-10)
        sys.add_window(make_win(
            20,
            200,
            r"C:\Apps\MT5\terminal64.exe",
            "MetaQuotes::MetaTrader::5.00",
            "MT5 Account A",
            "MON-2-TARGET",
        ));
        sys.add_window(make_win(
            21,
            201,
            r"C:\Apps\MT5\terminal64.exe",
            "MetaQuotes::MetaTrader::5.00",
            "MT5 Account B",
            "MON-2-TARGET",
        ));

        // One Edge window on MON-1-PRIMARY and one Edge window on MON-2-TARGET -> AmbiguousCrossMonitor (4.2 #8)
        sys.add_window(make_win(
            60,
            600,
            r"C:\Program Files\Microsoft\Edge\msedge.exe",
            "Chrome_WidgetWin_1",
            "Edge Mon 1",
            "MON-1-PRIMARY",
        ));
        sys.add_window(make_win(
            61,
            601,
            r"C:\Program Files\Microsoft\Edge\msedge.exe",
            "Chrome_WidgetWin_1",
            "Edge Mon 2",
            "MON-2-TARGET",
        ));

        let mut state_mgr = SceneStateManager::new(None);
        let mut resolver = BindingResolver::new();
        let jpath = test_journal_path("at10");
        let mut recovery = RecoveryManager::new(jpath.clone());

        let out = execute_scene_switch(
            "code",
            &cfg,
            &mut state_mgr,
            &mut resolver,
            &mut recovery,
            &mut sys,
            None,
        );
        // Neither ambiguous MT5 window nor unconfirmed cross-monitor Edge window is touched!
        assert!(out.minimized_hwnds.is_empty());
        assert!(out
            .warnings
            .contains(&SwitchWarning::AmbiguousBindingSameMonitor {
                binding_id: "mt5".to_string(),
                candidate_hwnds: vec![20, 21],
            }));
        assert!(out
            .warnings
            .contains(&SwitchWarning::AmbiguousBindingCrossMonitor {
                binding_id: "web-edge".to_string(),
                target_hwnd: 61,
            }));

        let _ = std::fs::remove_file(jpath);
    }

    #[test]
    fn test_at15_restore_all_and_at17_process_restart_and_at27_crash_recovery() {
        let cfg = setup_standard_config();
        let mut sys = FakeWindowSystem::new_dual_monitor();
        sys.add_window(make_win(
            20,
            200,
            r"C:\Apps\MT5\terminal64.exe",
            "MetaQuotes::MetaTrader::5.00",
            "MT5",
            "MON-2-TARGET",
        ));
        sys.add_window(make_win(
            30,
            300,
            r"C:\Apps\VSCode\Code.exe",
            "Chrome_WidgetWin_1",
            "Visual Studio Code",
            "MON-2-TARGET",
        ));

        let mut state_mgr = SceneStateManager::new(None);
        let mut resolver = BindingResolver::new();
        let jpath = test_journal_path("at15_17_27");
        let mut recovery = RecoveryManager::new(jpath.clone());

        // Switch to "code" -> minimizes MT5 (20) and writes runtime-state.json
        let _ = execute_scene_switch(
            "code",
            &cfg,
            &mut state_mgr,
            &mut resolver,
            &mut recovery,
            &mut sys,
            None,
        );
        assert!(sys.query_window(20).unwrap().is_iconic);

        // AT-27: Simulate crash and new process startup reading the surviving `runtime-state.json`
        let mut new_recovery = RecoveryManager::new(jpath.clone());
        let candidates = new_recovery.check_previous_crash_journal(&sys);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].hwnd, 20);

        // AT-15: "Restore All" restores only windows minimized by Kiri
        let restored = new_recovery.restore_all_minimized_by_us(&mut sys);
        assert_eq!(restored, 1);
        assert!(!sys.query_window(20).unwrap().is_iconic);

        // AT-17: Registered app (MT5) exits and a completely different app reuses HWND 20
        sys.windows.retain(|w| w.hwnd != 20);
        sys.add_window(make_win(
            20,
            999, // Different PID!
            r"C:\Windows\notepad.exe",
            "Notepad",
            "Reused HWND 20",
            "MON-2-TARGET",
        ));
        state_mgr.reset_to_unknown();
        let out = execute_scene_switch(
            "code",
            &cfg,
            &mut state_mgr,
            &mut resolver,
            &mut new_recovery,
            &mut sys,
            None,
        );
        // Must NOT minimize reused HWND 20 because its PID/start_time & EXE do not match MT5!
        assert!(out.minimized_hwnds.is_empty());
        assert!(!sys.query_window(20).unwrap().is_iconic);

        let _ = std::fs::remove_file(jpath);
    }

    #[test]
    fn test_at11_overflow_scenes_split() {
        let mut cfg = setup_standard_config();
        cfg.launcher.visible_slots = 3;
        cfg.scenes = vec![
            Scene { id: "s1".to_string(), name: "S1".to_string(), icon: "chart".to_string(), order: 1, pinned: true },
            Scene { id: "s2".to_string(), name: "S2".to_string(), icon: "code".to_string(), order: 2, pinned: true },
            Scene { id: "s3".to_string(), name: "S3".to_string(), icon: "globe".to_string(), order: 3, pinned: true },
            Scene { id: "s4".to_string(), name: "S4".to_string(), icon: "chart".to_string(), order: 4, pinned: true },
            Scene { id: "s5".to_string(), name: "S5".to_string(), icon: "chart".to_string(), order: 5, pinned: false },
        ];
        let (visible, overflow) = cfg.split_visible_and_overflow();
        assert_eq!(visible.len(), 3);
        assert_eq!(visible[0].id, "s1");
        assert_eq!(visible[1].id, "s2");
        assert_eq!(visible[2].id, "s3");
        assert_eq!(overflow.len(), 2);
        assert_eq!(overflow[0].id, "s4");
        assert_eq!(overflow[1].id, "s5");
    }

    #[test]
    fn test_at14_hung_window_does_not_block_and_emits_warning() {
        let cfg = setup_standard_config();
        let mut sys = FakeWindowSystem::new_dual_monitor();
        let mut mt5 = make_win(
            20,
            200,
            r"C:\Apps\MT5\terminal64.exe",
            "MetaQuotes::MetaTrader::5.00",
            "MT5",
            "MON-2-TARGET",
        );
        mt5.is_hung = true; // Hung window
        sys.add_window(mt5);

        let mut state_mgr = SceneStateManager::new(None);
        let mut resolver = BindingResolver::new();
        let jpath = test_journal_path("at14");
        let mut recovery = RecoveryManager::new(jpath.clone());

        // Switch to "code" -> MT5 is hung, but Kiri does not block, issues async minimize, emits warning
        let out = execute_scene_switch(
            "code",
            &cfg,
            &mut state_mgr,
            &mut resolver,
            &mut recovery,
            &mut sys,
            None,
        );
        assert!(out.warnings.contains(&SwitchWarning::HungWindowDetected {
            binding_id: "mt5".to_string(),
            hwnd: 20,
        }));
        assert_eq!(out.minimized_hwnds, vec![20]);

        let _ = std::fs::remove_file(jpath);
    }

    #[test]
    fn test_at24_maximized_window_retains_maximized_state_across_scene_switches() {
        let cfg = setup_standard_config();
        let mut sys = FakeWindowSystem::new_dual_monitor();
        let mut mt5 = make_win(
            20,
            200,
            r"C:\Apps\MT5\terminal64.exe",
            "MetaQuotes::MetaTrader::5.00",
            "MT5",
            "MON-2-TARGET",
        );
        mt5.is_maximized = true; // Started maximized
        sys.add_window(mt5);
        sys.add_window(make_win(
            30,
            300,
            r"C:\Apps\VSCode\Code.exe",
            "Chrome_WidgetWin_1",
            "Visual Studio Code",
            "MON-2-TARGET",
        ));

        let mut state_mgr = SceneStateManager::new(None);
        let mut resolver = BindingResolver::new();
        let jpath = test_journal_path("at24");
        let mut recovery = RecoveryManager::new(jpath.clone());

        // Switch to "code" -> MT5 (20) is minimized
        let _ = execute_scene_switch(
            "code",
            &cfg,
            &mut state_mgr,
            &mut resolver,
            &mut recovery,
            &mut sys,
            None,
        );
        assert!(sys.query_window(20).unwrap().is_iconic);

        // Switch back to "trade" -> MT5 (20) is restored with RestoreNoActivate
        let out = execute_scene_switch(
            "trade",
            &cfg,
            &mut state_mgr,
            &mut resolver,
            &mut recovery,
            &mut sys,
            None,
        );
        assert_eq!(out.restored_hwnds, vec![20]);
        let restored_win = sys.query_window(20).unwrap();
        assert!(!restored_win.is_iconic);
        // Maximized status must be preserved (AT-24)
        assert!(restored_win.is_maximized);

        let _ = std::fs::remove_file(jpath);
    }
}
