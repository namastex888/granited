import SwiftUI

// MARK: - General

struct GeneralPane: View {
    @ObservedObject var state: AppState

    var body: some View {
        Form {
            Section {
                HStack(spacing: 14) {
                    SettingsIcon(symbol: "cpu", tint: .accentColor, size: 52)
                    VStack(alignment: .leading, spacing: 3) {
                        Text("granited").font(.title2.weight(.semibold))
                        Text("Serves IBM Granite models on this Mac and across your nodes.")
                            .font(.callout).foregroundStyle(.secondary)
                    }
                }
                .padding(.vertical, 4)
            }

            RestartBanner(state: state)

            Section {
                LabeledContent("Status") { StatusText(text: state.engine.label, color: state.engine.color) }
                if case .unreachable(let why) = state.engine {
                    LabeledContent("Reason", value: why)
                }
                LabeledContent("Uptime", value: state.engine.isRunning ? Format.duration(state.uptime) : "—")
                LabeledContent("Host ID", value: state.hostID ?? "—")
                LabeledContent("Listen address", value: state.profile?.listen ?? "—")
                LabeledContent("Endpoint", value: state.config.url)
                LabeledContent("Engine") {
                    HStack {
                        Button("Start") { state.service(.start) }.disabled(state.busy || state.engine.isRunning)
                        Button("Stop") { state.service(.stop) }.disabled(state.busy || !state.engine.isRunning)
                        Button("Restart") { state.service(.restart) }.disabled(state.busy)
                    }
                }
            } footer: {
                LastActionFooter(state: state)
            }

            Section {
                Toggle("Start at login", isOn: Binding(
                    get: { state.startAtLogin ?? false },
                    set: { state.setStartAtLogin($0) }))
                    .disabled(state.startAtLogin == nil || state.busy)
                LabeledContent("Login item", value: state.config.label)
            } footer: {
                Text(state.startAtLogin == nil
                     ? "No LaunchAgent at \(state.config.plist). Install the engine with localfleet install."
                     : "Runs the engine as the LaunchAgent \(state.config.label) when you log in.")
                    .font(.caption).foregroundStyle(.secondary)
            }

            Section {
                LabeledContent("Logs") {
                    Button("Show in Finder") { state.showLogs() }.disabled(state.config.logs == nil)
                }
                LabeledContent("App configuration", value: state.config.source ?? "Not configured")
            } footer: {
                if state.config.dryRun {
                    Label("Dry run is on: engine, login and installation commands are shown, not executed.",
                          systemImage: "info.circle").font(.caption).foregroundStyle(.secondary)
                } else if let note = state.configNote {
                    Text(note).font(.caption).foregroundStyle(.secondary)
                }
            }
        }
    }
}

// MARK: - Models

struct ModelsPane: View {
    @ObservedObject var state: AppState

    var body: some View {
        Form {
            RestartBanner(state: state)
            if let error = state.modelsError {
                Section { Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(.secondary) }
            } else if state.workers.isEmpty {
                Section { Text("No models are deployed on this engine.").foregroundStyle(.secondary) }
            }
            ForEach(state.workers) { w in
                WorkerSection(state: state, worker: w)
            }
            if let error = state.profileError {
                Section { Text("Keep warm needs the profile: \(error)").font(.caption).foregroundStyle(.secondary) }
            }
        }
    }
}

struct WorkerSection: View {
    @ObservedObject var state: AppState
    let worker: Worker

    var body: some View {
        Section {
            LabeledContent("Family", value: state.family(of: worker.model).map { $0.replacingOccurrences(of: "_", with: " ").capitalized } ?? "—")
            LabeledContent("Device", value: [worker.device?.uppercased(), worker.backend].compactMap { $0 }.joined(separator: " · "))
            LabeledContent("State") { StatusText(text: stateLabel, color: stateColor) }
            LabeledContent("Requests", value: "\(worker.active_requests ?? 0) of \(worker.max_concurrency.map(String.init) ?? "—")")
            LabeledContent("Last load", value: Format.ms(worker.load_ms))
            if worker.isHeld {
                LabeledContent("Held") { Text(worker.hold_reason ?? "").foregroundStyle(.orange) }
            }
            if let e = worker.last_error {
                LabeledContent("Last error") { Text(e.summary).foregroundStyle(.secondary).lineLimit(3) }
            }
            Toggle("Keep warm", isOn: Binding(
                get: { state.profile?.keepWarm(worker.model) ?? false },
                set: { state.setKeepWarm(worker.model, $0) }))
                .disabled(!inProfile)
            LabeledContent {
                HStack {
                    if let op = operation, op.isRunning { ProgressView().controlSize(.small) }
                    Button("Load") { state.modelAction(.load, model: worker.model) }
                        .disabled(worker.isHeld || worker.state == "ready" || worker.state == "loading" || running)
                    Button("Unload") { state.modelAction(.unload, model: worker.model) }
                        .disabled(worker.state == "unloaded" || running)
                }
            } label: {
                Text(operation.map(\.summary) ?? "")
                    .font(.caption).foregroundStyle(.secondary).lineLimit(2)
            }
        } header: {
            Text(worker.model)
        } footer: {
            if worker.isHeld {
                Text("Held models cannot be loaded until the hold is removed from the profile.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    private var operation: Operation? { state.operations[worker.model] }
    private var running: Bool { operation?.isRunning ?? false }
    private var inProfile: Bool { state.profile?.deployments?.contains(where: { $0.model == worker.model }) ?? false }

    private var stateLabel: String { (worker.state ?? "unknown").capitalized }
    private var stateColor: Color {
        switch worker.state {
        case "ready": .green
        case "loading", "unloading": .orange
        case "failed": .red
        default: .secondary
        }
    }
}

// MARK: - Devices

struct DevicesPane: View {
    @ObservedObject var state: AppState

    var body: some View {
        Form {
            if let hw = state.hardware {
                Section("Processor") {
                    LabeledContent("Chip", value: hw.cpu_brand ?? "—")
                    LabeledContent("Logical CPUs", value: hw.logical_cpus.map(String.init) ?? "—")
                    LabeledContent("Architecture", value: hw.architecture ?? "—")
                    LabeledContent("Load average", value: loadAverage(hw.load_average))
                }
                Section("Memory") {
                    memoryGauge(hw.ram)
                    LabeledContent("Available", value: Format.bytes(hw.ram?.available_bytes))
                    LabeledContent("Swap", value: swap(hw.ram))
                    LabeledContent("Memory stall") {
                        let stall = hw.memory_stall_percent
                        HStack(spacing: 4) {
                            if let stall, stall > 5 { Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange) }
                            Text(stall.map { String(format: "%.0f%%", $0) } ?? "—")
                        }
                    }
                    LabeledContent("Reserved for macOS", value: Format.bytes(hw.admission?.ram_reserve_bytes))
                }
                Section("Accelerators") {
                    if (hw.accelerators ?? []).isEmpty {
                        Text("No accelerator detected").foregroundStyle(.secondary)
                    }
                    ForEach(Array((hw.accelerators ?? []).enumerated()), id: \.offset) { _, a in
                        LabeledContent(a.name ?? "Accelerator") {
                            VStack(alignment: .trailing, spacing: 2) {
                                Text(accelerator(a))
                                if let note = a.note { Text(note).font(.caption).foregroundStyle(.secondary) }
                            }
                        }
                    }
                }
            } else {
                Section {
                    Label(state.hardwareError ?? "Loading…", systemImage: "exclamationmark.triangle")
                        .foregroundStyle(.secondary)
                }
            }
            HardwarePlanSections(state: state)
        }
    }

    @ViewBuilder
    private func memoryGauge(_ ram: Hardware.RAM?) -> some View {
        if let total = ram?.total_bytes, let avail = ram?.available_bytes, total > 0 {
            let used = max(0, total - avail)
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Text("Memory")
                    Spacer()
                    Text("\(Format.bytes(used)) of \(Format.bytes(total)) used").foregroundStyle(.secondary)
                }
                Gauge(value: Double(used), in: 0...Double(total)) { EmptyView() }
                    .gaugeStyle(.linearCapacity)
                    .tint(Double(used) / Double(total) > 0.9 ? .orange : .accentColor)
            }
        } else {
            LabeledContent("Memory", value: "—")
        }
    }

    private func loadAverage(_ l: Hardware.LoadAverage?) -> String {
        guard let l else { return "—" }
        return [l.one, l.five, l.fifteen].map { $0.map { String(format: "%.2f", $0) } ?? "—" }.joined(separator: ", ")
    }

    private func swap(_ r: Hardware.RAM?) -> String {
        guard let total = r?.swap_total_bytes, total > 0 else { return "None in use" }
        return "\(Format.bytes(r?.swap_used_bytes ?? 0)) of \(Format.bytes(total)) used"
    }

    private func accelerator(_ a: Hardware.Accelerator) -> String {
        var parts: [String] = []
        if let v = a.vendor { parts.append(v.capitalized) }
        if let c = a.cores?.description, !c.isEmpty { parts.append("\(c) cores") }
        if let k = a.memory_kind { parts.append("\(k) memory") }
        if let t = a.total_memory_bytes { parts.append(Format.bytes(t)) }
        return parts.joined(separator: " · ")
    }
}

/// "Hardware plan" from `localfleet scan --json`: what this machine can deploy and, for every
/// device it cannot use, why.
struct HardwarePlanSections: View {
    @ObservedObject var state: AppState

    var body: some View {
        Section {
            if let plan = state.scan?.plan {
                LabeledContent("Language runtime", value: plan.llama.map { "llama.cpp on \($0.device ?? "?") (\($0.pool ?? "?"))" } ?? "None")
                LabeledContent("Language models", value: languages(plan.languages))
                LabeledContent("Native workers", value: (plan.native ?? []).isEmpty ? "None" : (plan.native ?? []).map(\.capitalized).joined(separator: ", ") + " (CPU)")
                LabeledContent("Reserved for the OS", value: plan.reserve_gib.map { String(format: "%.0f GiB", $0) } ?? "—")
            } else if let error = state.scanError {
                Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(.secondary)
            } else {
                HStack { ProgressView().controlSize(.small); Text("Scanning this Mac…").foregroundStyle(.secondary) }
            }
        } header: {
            HStack {
                Text("Hardware plan")
                Spacer()
                Button("Scan Again") { state.runScan() }
                    .controlSize(.small)
                    .disabled(state.scanning)
            }
        } footer: {
            Text("From localfleet scan. Scanning only inventories this Mac; it starts and installs nothing.")
                .font(.caption).foregroundStyle(.secondary)
        }
        .onAppear { if state.scan == nil { state.runScan() } }

        if let plan = state.scan?.plan {
            Section("Skipped devices") {
                if (plan.skipped ?? []).isEmpty {
                    Text("None: every detected device has a use in the plan.").foregroundStyle(.secondary)
                }
                ForEach(plan.skipped ?? [], id: \.self) { line in
                    Label(line, systemImage: "minus.circle").foregroundStyle(.primary)
                }
            }
            if !(plan.notes ?? []).isEmpty {
                Section("Notes") {
                    ForEach(plan.notes ?? [], id: \.self) { Text($0).font(.callout) }
                }
            }
        }
    }

    private func languages(_ sizes: [String]?) -> String {
        guard let sizes, !sizes.isEmpty else { return "None" }
        return "Granite 4.2 " + sizes.map { $0.uppercased() }.joined(separator: ", ")
    }
}

// MARK: - Nodes

struct NodesPane: View {
    @ObservedObject var state: AppState
    @State private var showAdd = false
    @State private var command: StreamingCommand?
    @State private var error: String?

    var body: some View {
        Form {
            Section {
                if state.hosts.isEmpty {
                    Text(state.hostsError.map { "Unavailable: \($0)" } ?? "Loading…").foregroundStyle(.secondary)
                }
                ForEach(Array(state.hosts.enumerated()), id: \.element.id) { index, h in
                    HStack(spacing: 10) {
                        SettingsIcon(symbol: index == 0 ? "laptopcomputer" : "server.rack",
                                     tint: (h.reachable ?? false) ? .indigo : .gray, size: 26)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(h.host)
                            Text(index == 0 ? "This Mac" : (peerURL(h.host) ?? "Peer"))
                                .font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer()
                        VStack(alignment: .trailing, spacing: 1) {
                            StatusText(text: (h.reachable ?? false) ? "Reachable" : "Unreachable",
                                       color: (h.reachable ?? false) ? .green : .red)
                            Text(detail(h, local: index == 0)).font(.caption).foregroundStyle(.secondary)
                        }
                        if index > 0 {
                            Button("Remove…") { remove(h.host) }
                        }
                    }
                }
            } header: {
                Text("Nodes")
            } footer: {
                LastActionFooter(state: state)
            }
            Section {
                LabeledContent("Add a machine over SSH") {
                    Button("Add Node…") { showAdd = true }
                }
            } footer: {
                Text("localfleet ships this checkout to the machine, installs it there, links it to this engine and enrolls it.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            if let error {
                Section { Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(.secondary) }
            }
        }
        .sheet(isPresented: $showAdd) {
            AddNodeSheet(state: state) { showAdd = false; Task { await state.refresh() } }
        }
        .sheet(item: $command) { cmd in
            CommandSheet(command: cmd) { command = nil; Task { await state.refresh() } }
        }
    }

    private func peerURL(_ id: String) -> String? { state.peers.first(where: { $0.id == id })?.url }

    private func detail(_ h: HostStatus, local: Bool) -> String {
        var parts: [String] = []
        if let rtt = h.round_trip_ms { parts.append(Format.ms(rtt)) } else if local { parts.append("local") }
        if let n = h.modelCount { parts.append(plural(n, "model")) }
        if h.reachable == false {
            if let s = h.status { parts.append("HTTP \(s)") } else if let e = h.error { parts.append(String(e.prefix(60))) }
        }
        return parts.joined(separator: " · ")
    }

    private func remove(_ name: String) {
        do {
            let spec = try state.cli().nodeRemove(name: name)
            guard state.confirm("Remove \(name)?",
                                detail: "Runs: \(spec.commandLine)\nThe node is unlinked from this engine and the engine restarts. The node's own service keeps running.",
                                button: "Remove", destructive: true) else { return }
            command = state.makeCommand(title: "Remove \(name)", spec: spec)
            error = nil
        } catch {
            self.error = String(describing: error)
        }
    }
}

struct AddNodeSheet: View {
    @ObservedObject var state: AppState
    var onClose: () -> Void

    @State private var destination = ""
    @State private var name = ""
    @State private var jump = ""
    @State private var command: StreamingCommand?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                Section {
                    TextField("SSH destination", text: $destination, prompt: Text("user@gpu-box.local"))
                    TextField("Name", text: $name, prompt: Text("Optional"))
                    TextField("Jump host", text: $jump, prompt: Text("Optional"))
                } header: {
                    Text("Add Node")
                } footer: {
                    Text(preview).font(.system(.caption, design: .monospaced)).foregroundStyle(.secondary).textSelection(.enabled)
                }
            }
            .formStyle(.grouped)
            .disabled(command != nil)
            .frame(height: 230)

            if let command {
                CommandOutput(command: command).padding(.horizontal, 20)
            }

            HStack {
                if state.config.dryRun {
                    Label("Dry run", systemImage: "info.circle").font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
                if let command, command.running {
                    Button("Cancel", role: .cancel) { command.cancel() }
                } else if command?.finished == true {
                    Button("Done") { onClose() }.keyboardShortcut(.defaultAction)
                } else {
                    Button("Cancel", role: .cancel) { onClose() }.keyboardShortcut(.cancelAction)
                    Button("Add Node") { start() }
                        .keyboardShortcut(.defaultAction)
                        .disabled(spec == nil)
                }
            }
            .padding(20)
        }
        .frame(width: 560)
    }

    private var spec: CommandSpec? {
        try? state.cli().nodeAdd(destination: destination, name: name, jump: jump)
    }

    private var preview: String {
        do { return try state.cli().nodeAdd(destination: destination, name: name, jump: jump).commandLine }
        catch { return destination.isEmpty ? "Enter the SSH destination of the new machine." : String(describing: error) }
    }

    private func start() {
        guard let spec else { return }
        let cmd = state.makeCommand(title: "Add \(destination)", spec: spec)
        command = cmd
        cmd.start()
    }
}

/// The live log of a command inside a sheet.
struct CommandOutput: View {
    @ObservedObject var command: StreamingCommand
    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                if command.running { ProgressView().controlSize(.small) }
                Text(command.running ? "Running…" : (command.exitStatus == 0 ? "Finished" : "Failed (exit \(command.exitStatus ?? -1))"))
                    .font(.caption).foregroundStyle(.secondary)
            }
            LogView(text: command.log).frame(height: 200)
        }
    }
}

// MARK: - Connect

struct ConnectPane: View {
    @ObservedObject var state: AppState
    @State private var revealed = false

    var body: some View {
        Form {
            Section {
                LabeledContent("Base URL") {
                    HStack {
                        Text(state.config.openAIBaseURL).textSelection(.enabled)
                        Button("Copy") { state.copy(state.config.openAIBaseURL) }
                    }
                }
                LabeledContent("API key") {
                    HStack {
                        Text(revealed ? (state.apiKey() ?? "") : String(repeating: "•", count: 16))
                            .font(.system(.body, design: .monospaced))
                            .textSelection(.enabled)
                            .lineLimit(1).truncationMode(.middle)
                        Button(revealed ? "Hide" : "Reveal") { revealed.toggle() }
                        Button("Copy") { if let k = state.apiKey() { state.copy(k) } }
                    }
                    .disabled(!state.hasAPIKey)
                }
                LabeledContent("Model", value: state.snippetModel)
            } header: {
                Text("OpenAI-compatible API")
            } footer: {
                Text(state.hasAPIKey ? "Any OpenAI client works with this base URL and key."
                     : "No GRANITE_API_KEY in the keys file (set `keys` in app.json).")
                    .font(.caption).foregroundStyle(.secondary)
            }
            snippet("curl", curl)
            snippet("Python (openai)", python)
        }
        .onDisappear { revealed = false }
    }

    private func snippet(_ title: String, _ code: String) -> some View {
        Section {
            Text(code)
                .font(.system(.callout, design: .monospaced))
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
        } header: {
            HStack {
                Text(title)
                Spacer()
                Button("Copy") { state.copy(code) }.controlSize(.small)
            }
        } footer: {
            Text("Export GRANITE_API_KEY with the key above first.").font(.caption).foregroundStyle(.secondary)
        }
    }

    private var curl: String {
        """
        curl \(state.config.openAIBaseURL)/chat/completions \\
          -H "Authorization: Bearer $GRANITE_API_KEY" \\
          -H "Content-Type: application/json" \\
          -d '{"model": "\(state.snippetModel)", "messages": [{"role": "user", "content": "Hello"}]}'
        """
    }

    private var python: String {
        """
        import os
        from openai import OpenAI

        client = OpenAI(base_url="\(state.config.openAIBaseURL)", api_key=os.environ["GRANITE_API_KEY"])
        reply = client.chat.completions.create(
            model="\(state.snippetModel)",
            messages=[{"role": "user", "content": "Hello"}],
        )
        print(reply.choices[0].message.content)
        """
    }
}

// MARK: - Advanced

struct AdvancedPane: View {
    @ObservedObject var state: AppState
    @State private var command: StreamingCommand?
    @State private var error: String?

    private static let gib: Int64 = 1_073_741_824

    var body: some View {
        Form {
            RestartBanner(state: state)
            Section {
                LabeledContent("Profile") {
                    Text(state.config.profile ?? "Not configured").lineLimit(1).truncationMode(.middle).textSelection(.enabled)
                }
                LabeledContent("") {
                    HStack {
                        Button("Reveal in Finder") { state.reveal(state.config.profile) }
                        Button("Open") { state.open(state.config.profile) }
                    }
                    .disabled(state.config.profile == nil)
                }
                Stepper(value: reserveBinding, in: 0...max(64, reserveGiB + 8)) {
                    LabeledContent("Memory reserve", value: state.profile?.ram_reserve_bytes == nil ? "—" : "\(reserveGiB) GiB")
                }
                .disabled(state.profile == nil)
            } header: {
                Text("Engine profile")
            } footer: {
                Text(state.profileError ?? "RAM the engine keeps free for macOS and other apps before it loads a model. Changes apply after a restart.")
                    .font(.caption).foregroundStyle(.secondary)
            }

            Section("Locations") {
                path("Checkout", state.config.root)
                path("Models directory", state.config.models)
                path("Logs", state.config.logs)
                path("Python", state.config.python)
                path("localfleet", state.config.cli)
                path("App configuration", state.config.source)
            }

            Section {
                LabeledContent("Remove the login services") {
                    Button("Uninstall Engine…", role: .destructive) { uninstall() }
                }
            } footer: {
                Text(error ?? "Runs localfleet uninstall: stops and removes the engine's login services. Models, keys and files stay.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
        .sheet(item: $command) { cmd in
            CommandSheet(command: cmd) { command = nil; Task { await state.refresh() } }
        }
    }

    private var reserveGiB: Int { Int(((state.profile?.ram_reserve_bytes ?? 0) + Self.gib / 2) / Self.gib) }

    private var reserveBinding: Binding<Int> {
        Binding(get: { reserveGiB }, set: { state.setRamReserve(gib: $0) })
    }

    private func path(_ label: String, _ value: String?) -> some View {
        LabeledContent(label) {
            HStack {
                Text(value ?? "Not configured").lineLimit(1).truncationMode(.middle)
                    .foregroundStyle(value == nil ? .secondary : .primary)
                    .textSelection(.enabled)
                if let value, FileManager.default.fileExists(atPath: value) {
                    Button {
                        state.reveal(value)
                    } label: {
                        Image(systemName: "arrow.right.circle.fill")
                    }
                    .buttonStyle(.borderless)
                    .help("Reveal in Finder")
                }
            }
        }
    }

    private func uninstall() {
        do {
            let spec = try state.cli().uninstall()
            guard state.confirm("Uninstall the engine?",
                                detail: "Runs: \(spec.commandLine)\nThis stops the engine and its node links and removes their login services. Connected consumers lose access. Models, keys and files stay in the checkout.",
                                button: "Uninstall", destructive: true) else { return }
            command = state.makeCommand(title: "Uninstall Engine", spec: spec)
            error = nil
        } catch {
            self.error = String(describing: error)
        }
    }
}
