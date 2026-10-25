import Foundation

let arguments = Array(CommandLine.arguments.dropFirst())
LaunchOptions.current = LaunchOptions.parse(arguments)

if arguments.contains("--help") || arguments.contains("-h") {
    print("""
    usage: GraniteBar [--config PATH] [--open-preferences] [--pane NAME] [--dry-run]
               run the menu-bar app; --pane is general|models|devices|nodes|connect|advanced
           GraniteBar --self-test [--url URL] [--keys PATH] [--config PATH]
           GraniteBar --dry-run-node-add DEST [--name N] [--jump J]
           GraniteBar --dry-run-node-remove NAME | --dry-run-uninstall
           GraniteBar --write-keep-warm MODEL on|off --profile PATH
           GraniteBar --write-ram-reserve GIB --profile PATH
           GraniteBar --write-run-at-load on|off --plist PATH
           GraniteBar --service start|stop|restart [--config PATH]   (not for ai.granited.router)
           GraniteBar --scan
           GraniteBar --render-icons DIR                  (menu-bar icon per state, PNG)
           GraniteBar --exercise-load MODEL --url URL     (refuses port 8310)
    config: \(AppConfig.defaultPath) (override with GRANITE_BAR_CONFIG or --config)
    """)
} else if SelfTest.modes.contains(where: arguments.contains) {
    // Headless: no NSApplication, no menu bar.
    Task { @MainActor in
        let code = await SelfTest.run(arguments)
        exit(code)
    }
    dispatchMain()
} else {
    MainActor.assumeIsolated { GraniteBarApp.main() }
}
