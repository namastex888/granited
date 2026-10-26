import Foundation

let arguments = Array(CommandLine.arguments.dropFirst())
LaunchOptions.current = LaunchOptions.parse(arguments)

if arguments.contains("--help") || arguments.contains("-h") {
    print("""
    usage: LocalFleet [--config PATH] [--open-preferences] [--pane NAME] [--dry-run]
               run the menu-bar app; --pane is general|models|devices|nodes|connect|advanced
           LocalFleet --self-test [--url URL] [--keys PATH] [--config PATH]
           LocalFleet --dry-run-node-add DEST [--name N] [--jump J]
           LocalFleet --dry-run-node-remove NAME | --dry-run-uninstall
           LocalFleet --write-keep-warm MODEL on|off --profile PATH
           LocalFleet --write-ram-reserve GIB --profile PATH
           LocalFleet --write-run-at-load on|off --plist PATH
           LocalFleet --service start|stop|restart [--config PATH]   (not for ai.granited.router)
           LocalFleet --scan
           LocalFleet --render-icons DIR                  (menu-bar icon per state, PNG)
           LocalFleet --exercise-load MODEL --url URL     (refuses port 8310)
    config: \(AppConfig.defaultPath) (override with LOCALFLEET_CONFIG or --config)
    """)
} else if SelfTest.modes.contains(where: arguments.contains) {
    // Headless: no NSApplication, no menu bar.
    Task { @MainActor in
        let code = await SelfTest.run(arguments)
        exit(code)
    }
    dispatchMain()
} else {
    MainActor.assumeIsolated { LocalFleetApp.main() }
}
