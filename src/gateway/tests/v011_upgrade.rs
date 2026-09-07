    #[test]
    fn older_journal_versions_are_rejected_without_touching_rows() {
        for version in [0_u32, 1, 2, 3, 5] {
            let path = database_path(&format!("old-version-{version}"));
            let operation = request();
            {
                let gateway = Gateway::open_for_test(&path).unwrap();
                gateway
                    .submit_exact_for_test(&operation, &authorization(&operation))
                    .unwrap();
                gateway
                    .journal
                    .connection
                    .pragma_update(None, "user_version", version)
                    .unwrap();
            }
            let before = fs::read(&path).unwrap();
            assert!(matches!(
                Gateway::open_for_test(&path),
                Err(GatewayError::UnsupportedJournalVersion)
            ));
            assert_eq!(fs::read(&path).unwrap(), before);
            fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }
    }
