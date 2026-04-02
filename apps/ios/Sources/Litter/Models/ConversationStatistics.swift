import Foundation

struct ConversationStatistics {
    var totalMessages: Int = 0
    var userMessageCount: Int = 0
    var assistantMessageCount: Int = 0
    var turnCount: Int = 0
    var commandsExecuted: Int = 0
    var commandsSucceeded: Int = 0
    var commandsFailed: Int = 0
    var filesChanged: Int = 0
    var totalCommandDurationMs: Int64 = 0
    var mcpToolCallCount: Int = 0
    var webSearchCount: Int = 0

    static func compute(from items: [HydratedConversationItem]) -> ConversationStatistics {
        var stats = ConversationStatistics()
        var seenTurnIds = Set<String>()

        for item in items {
            switch item.content {
            case .user:
                stats.userMessageCount += 1
                stats.totalMessages += 1
            case .assistant:
                stats.assistantMessageCount += 1
                stats.totalMessages += 1
            case .codeReview:
                stats.assistantMessageCount += 1
                stats.totalMessages += 1
            case .commandExecution(let data):
                stats.commandsExecuted += 1
                switch data.status {
                case .completed:
                    stats.commandsSucceeded += 1
                case .failed:
                    stats.commandsFailed += 1
                default:
                    break
                }
                if let duration = data.durationMs {
                    stats.totalCommandDurationMs += duration
                }
            case .fileChange:
                stats.filesChanged += 1
            case .mcpToolCall:
                stats.mcpToolCallCount += 1
            case .webSearch:
                stats.webSearchCount += 1
            default:
                break
            }

            if let turnId = item.sourceTurnId {
                seenTurnIds.insert(turnId)
            }
        }

        stats.turnCount = seenTurnIds.count

        return stats
    }
}

struct ServerUsageData {
    struct TokenEntry: Identifiable {
        let id = UUID()
        let threadTitle: String
        let tokens: UInt64
    }

    struct ActivityEntry: Identifiable {
        let id = UUID()
        let date: Date
        let turnCount: Int
    }

    struct ModelEntry: Identifiable {
        let id = UUID()
        let model: String
        let threadCount: Int
    }

    var tokensByThread: [TokenEntry] = []
    var activityByDay: [ActivityEntry] = []
    var modelUsage: [ModelEntry] = []
    var rateLimits: RateLimitSnapshot?

    static func compute(from threads: [AppThreadSnapshot], server: AppServerSnapshot) -> ServerUsageData {
        var data = ServerUsageData()
        data.rateLimits = server.rateLimits

        // Token usage by thread
        data.tokensByThread = threads.compactMap { thread in
            guard let tokens = thread.contextTokensUsed, tokens > 0 else { return nil }
            let title = thread.displayTitle
            return TokenEntry(threadTitle: title, tokens: tokens)
        }.sorted { $0.tokens > $1.tokens }

        // Activity by day — aggregate thread creation/update timestamps
        var dayBuckets: [String: Int] = [:]
        let dayFormatter = DateFormatter()
        dayFormatter.dateFormat = "yyyy-MM-dd"

        for thread in threads {
            // Use updatedAt preferring over createdAt
            let ts = thread.info.updatedAt ?? thread.info.createdAt
            guard let ts else { continue }
            let date = Date(timeIntervalSince1970: TimeInterval(ts))
            let key = dayFormatter.string(from: date)
            dayBuckets[key, default: 0] += 1
        }

        data.activityByDay = dayBuckets.compactMap { key, count in
            guard let date = dayFormatter.date(from: key) else { return nil }
            return ActivityEntry(date: date, turnCount: count)
        }.sorted { $0.date < $1.date }

        // Model usage breakdown
        var modelCounts: [String: Int] = [:]
        for thread in threads {
            let model = thread.model ?? thread.info.model ?? "unknown"
            modelCounts[model, default: 0] += 1
        }

        data.modelUsage = modelCounts.map { model, count in
            ModelEntry(model: model, threadCount: count)
        }.sorted { $0.threadCount > $1.threadCount }

        return data
    }
}
