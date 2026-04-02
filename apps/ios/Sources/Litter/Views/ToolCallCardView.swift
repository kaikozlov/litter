import SwiftUI

struct ToolCallCardView: View {
    let model: ToolCallCardModel
    @State private var expanded: Bool

    init(model: ToolCallCardModel) {
        self.model = model
        _expanded = State(initialValue: model.defaultExpanded)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                Image(systemName: model.kind.iconName)
                    .litterFont(size: 12, weight: .semibold)
                    .foregroundColor(kindAccent)

                if let attributedSummary = model.attributedSummary {
                    Text(attributedSummary)
                        .litterFont(.caption)
                        .lineLimit(1)
                } else {
                    Text(model.summary)
                        .litterFont(.caption)
                        .foregroundColor(LitterTheme.textSystem)
                        .lineLimit(1)
                }

                Spacer()

                if let duration = model.duration, !duration.isEmpty {
                    Text(duration)
                        .litterFont(.caption2)
                        .foregroundColor(durationStatusColor)
                        .accessibilityLabel(durationAccessibilityLabel(duration))
                }

                Image(systemName: expanded ? "chevron.up" : "chevron.down")
                    .litterFont(size: 11, weight: .medium)
                    .foregroundColor(LitterTheme.textMuted)
            }
            .contentShape(Rectangle())
            .onTapGesture {
                withAnimation(.easeInOut(duration: 0.2)) {
                    expanded.toggle()
                }
            }

            if expanded {
                VStack(alignment: .leading, spacing: 10) {
                    ForEach(identifiedSections) { section in
                        sectionView(section.value)
                    }
                }
                .padding(.top, 8)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .onChange(of: model.status) { _, newStatus in
            if newStatus == .failed {
                expanded = true
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var durationStatusColor: Color {
        switch model.status {
        case .completed:
            return LitterTheme.success
        case .inProgress:
            return LitterTheme.warning
        case .failed:
            return LitterTheme.danger
        case .unknown:
            return LitterTheme.textSecondary
        }
    }

    private func durationAccessibilityLabel(_ duration: String) -> String {
        switch model.status {
        case .completed:
            return "\(duration), completed"
        case .inProgress:
            return "\(duration), in progress"
        case .failed:
            return "\(duration), failed"
        case .unknown:
            return duration
        }
    }

    private var kindAccent: Color {
        switch model.kind {
        case .commandExecution, .commandOutput:
            return LitterTheme.warning
        case .fileChange, .fileDiff, .webSearch:
            return LitterTheme.accent
        case .mcpToolCall, .widget:
            return LitterTheme.accentStrong
        case .mcpToolProgress, .imageView:
            return LitterTheme.warning
        case .collaboration:
            return LitterTheme.success
        }
    }

    @ViewBuilder
    private func sectionView(_ section: ToolCallSection) -> some View {
        switch section {
        case .kv(let label, let entries):
            if !entries.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    sectionLabel(label)
                    VStack(alignment: .leading, spacing: 4) {
                        ForEach(identifiedKeyValueEntries(entries)) { entry in
                            HStack(alignment: .top, spacing: 8) {
                                Text(entry.value.key + ":")
                                    .litterFont(.caption2, weight: .semibold)
                                    .foregroundColor(LitterTheme.textSecondary)
                                Text(entry.value.value)
                                    .litterFont(.caption2)
                                    .foregroundColor(LitterTheme.textSystem)
                                    .textSelection(.enabled)
                                Spacer(minLength: 0)
                            }
                        }
                    }
                    .padding(8)
                    .background(LitterTheme.surface.opacity(0.6))
                    .clipShape(RoundedRectangle(cornerRadius: 8))
                }
            }
        case .code(let label, let language, let content):
            codeLikeSection(label: label, language: language, content: content)
        case .json(let label, let content):
            codeLikeSection(label: label, language: "json", content: content)
        case .diff(let label, let content):
            diffSection(label: label, content: content)
        case .text(let label, let content):
            inlineTextSection(label: label, content: content)
        case .list(let label, let items):
            if !items.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    sectionLabel(label)
                    VStack(alignment: .leading, spacing: 4) {
                        ForEach(identifiedTextItems(items, prefix: "list")) { item in
                            HStack(alignment: .top, spacing: 6) {
                                Text("•")
                                    .litterFont(.caption)
                                    .foregroundColor(LitterTheme.textSecondary)
                                Text(item.value)
                                    .litterFont(.caption)
                                    .foregroundColor(LitterTheme.textSystem)
                                    .textSelection(.enabled)
                            }
                        }
                    }
                    .padding(8)
                    .background(LitterTheme.surface.opacity(0.6))
                    .clipShape(RoundedRectangle(cornerRadius: 8))
                }
            }
        case .progress(let label, let items):
            if !items.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    sectionLabel(label)
                    VStack(alignment: .leading, spacing: 6) {
                        let identifiedItems = identifiedTextItems(items, prefix: "progress")
                        ForEach(identifiedItems) { item in
                            HStack(alignment: .top, spacing: 8) {
                                Circle()
                                    .fill(item.index == identifiedItems.count - 1 ? kindAccent : LitterTheme.textMuted)
                                    .frame(width: 6, height: 6)
                                    .padding(.top, 5)
                                Text(item.value)
                                    .litterFont(.caption)
                                    .foregroundColor(LitterTheme.textSystem)
                                    .textSelection(.enabled)
                                Spacer(minLength: 0)
                            }
                        }
                    }
                    .padding(8)
                    .background(LitterTheme.surface.opacity(0.6))
                    .clipShape(RoundedRectangle(cornerRadius: 8))
                }
            }
        }
    }

    private func sectionLabel(_ label: String) -> some View {
        Text(label.uppercased())
            .litterFont(.caption2, weight: .bold)
            .foregroundColor(LitterTheme.textSecondary)
    }

    private func codeLikeSection(label: String, language: String, content: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            sectionLabel(label)
            CodeBlockView(language: language, code: content, fontSize: 13)
        }
    }

    private func inlineTextSection(label: String, content: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            sectionLabel(label)
            Text(verbatim: content)
                .litterMonoFont(size: 12)
                .foregroundColor(LitterTheme.textBody)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 10)
                .padding(.vertical, 8)
                .background(LitterTheme.codeBackground.opacity(0.72))
                .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private func diffSection(label: String, content: String) -> some View {
        let lines = content.components(separatedBy: .newlines)

        return VStack(alignment: .leading, spacing: 6) {
            sectionLabel(label)
            LazyVStack(alignment: .leading, spacing: 2) {
                ForEach(Array(lines.enumerated()), id: \.offset) { _, line in
                    Text(verbatim: line.isEmpty ? " " : line)
                        .litterMonoFont(size: 12)
                        .foregroundStyle(diffLineColor(for: line))
                        .lineLimit(1)
                        .truncationMode(.tail)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 4)
                        .background(diffLineBgColor(for: line))
                        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                }
            }
        }
    }

    private func diffLineColor(for line: String) -> Color {
        if line.hasPrefix("+"), !line.hasPrefix("+++") { return LitterTheme.success }
        if line.hasPrefix("-"), !line.hasPrefix("---") { return LitterTheme.danger }
        if line.hasPrefix("@@") { return LitterTheme.accentStrong }
        return LitterTheme.textBody
    }

    private func diffLineBgColor(for line: String) -> Color {
        if line.hasPrefix("+"), !line.hasPrefix("+++") { return LitterTheme.success.opacity(0.12) }
        if line.hasPrefix("-"), !line.hasPrefix("---") { return LitterTheme.danger.opacity(0.12) }
        if line.hasPrefix("@@") { return LitterTheme.accentStrong.opacity(0.12) }
        return LitterTheme.codeBackground.opacity(0.72)
    }


    private var identifiedSections: [IndexedValue<ToolCallSection>] {
        identifiedValues(model.sections, prefix: "section") { section in
            switch section {
            case .kv(let label, let entries):
                return "\(label)|kv|\(entries.map { "\($0.key)=\($0.value)" }.joined(separator: "|"))"
            case .code(let label, let language, let content):
                return "\(label)|code|\(language)|\(content)"
            case .json(let label, let content):
                return "\(label)|json|\(content)"
            case .diff(let label, let content):
                return "\(label)|diff|\(content)"
            case .text(let label, let content):
                return "\(label)|text|\(content)"
            case .list(let label, let items):
                return "\(label)|list|\(items.joined(separator: "|"))"
            case .progress(let label, let items):
                return "\(label)|progress|\(items.joined(separator: "|"))"
            }
        }
    }

    private func identifiedKeyValueEntries(_ entries: [ToolCallKeyValue]) -> [IndexedValue<ToolCallKeyValue>] {
        identifiedValues(entries, prefix: "kv") { entry in
            "\(entry.key)|\(entry.value)"
        }
    }

    private func identifiedTextItems(_ values: [String], prefix: String) -> [IndexedValue<String>] {
        identifiedValues(values, prefix: prefix) { $0 }
    }

    private func identifiedValues<Value>(
        _ values: [Value],
        prefix: String,
        key: (Value) -> String
    ) -> [IndexedValue<Value>] {
        var seen: [String: Int] = [:]
        return values.enumerated().map { index, value in
            let signature = key(value)
            let occurrence = seen[signature, default: 0]
            seen[signature] = occurrence + 1
            return IndexedValue(
                id: "\(prefix)-\(signature.hashValue)-\(occurrence)",
                index: index,
                value: value
            )
        }
    }
}

private struct IndexedValue<Value>: Identifiable {
    let id: String
    let index: Int
    let value: Value
}

#if DEBUG
#Preview("Tool Call Card") {
    ZStack {
        LitterTheme.backgroundGradient.ignoresSafeArea()
        ToolCallCardView(model: LitterPreviewData.sampleToolCallModel)
            .padding(20)
    }
}
#endif
