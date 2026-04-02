import Foundation

extension AppThreadSnapshot {
    var displayTitle: String {
        let explicitTitle = info.title?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if !explicitTitle.isEmpty {
            return explicitTitle
        }

        let preview = info.preview?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if !preview.isEmpty {
            return preview
        }

        return "Untitled session"
    }

    var hasActiveTurn: Bool {
        if activeTurnId != nil {
            return true
        }
        if case .active = info.status {
            return true
        }
        return false
    }

    var resolvedModel: String {
        let direct = model?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if !direct.isEmpty { return direct }
        let infoModel = info.model?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return infoModel
    }

    var resolvedPreview: String {
        displayTitle
    }

    var contextPercent: Int {
        guard let used = contextTokensUsed,
              let window = modelContextWindow,
              window > 0 else {
            return 0
        }
        return min(100, Int(Double(used) / Double(window) * 100))
    }

    var latestAssistantSnippet: String? {
        let text = hydratedConversationItems
            .map(\.conversationItem)
            .reversed()
            .compactMap { item -> String? in
                switch item.content {
                case .assistant(let data):
                    return data.text
                case .codeReview(let data):
                    return data.findings.first?.title
                default:
                    return nil
                }
            }
            .first?
            .prefix(120) ?? ""
        let snippet = String(text)
            .replacingOccurrences(of: "\n", with: " ")
            .trimmingCharacters(in: .whitespacesAndNewlines)
        return snippet.isEmpty ? nil : snippet
    }
}
