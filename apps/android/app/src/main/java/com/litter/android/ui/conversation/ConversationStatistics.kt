package com.litter.android.ui.conversation

import com.litter.android.state.displayTitle
import uniffi.codex_mobile_client.AppOperationStatus
import uniffi.codex_mobile_client.AppServerSnapshot
import uniffi.codex_mobile_client.AppThreadSnapshot
import uniffi.codex_mobile_client.HydratedConversationItem
import uniffi.codex_mobile_client.HydratedConversationItemContent
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId

data class ConversationStatistics(
    val totalMessages: Int = 0,
    val userMessageCount: Int = 0,
    val assistantMessageCount: Int = 0,
    val turnCount: Int = 0,
    val commandsExecuted: Int = 0,
    val commandsSucceeded: Int = 0,
    val commandsFailed: Int = 0,
    val filesChanged: Int = 0,
    val totalCommandDurationMs: Long = 0,
    val mcpToolCallCount: Int = 0,
    val webSearchCount: Int = 0,
) {
    companion object {
        fun compute(items: List<HydratedConversationItem>): ConversationStatistics {
            var userMessages = 0
            var assistantMessages = 0
            var turns = 0
            var cmdsExecuted = 0
            var cmdsSucceeded = 0
            var cmdsFailed = 0
            var files = 0
            var cmdDurationMs = 0L
            var mcpCalls = 0
            var webSearches = 0

            var lastWasUser = false
            for (item in items) {
                when (val content = item.content) {
                    is HydratedConversationItemContent.User -> {
                        userMessages++
                        if (!lastWasUser) turns++
                        lastWasUser = true
                    }
                    is HydratedConversationItemContent.Assistant -> {
                        assistantMessages++
                        lastWasUser = false
                    }
                    is HydratedConversationItemContent.CodeReview -> {
                        assistantMessages++
                        lastWasUser = false
                    }
                    is HydratedConversationItemContent.CommandExecution -> {
                        cmdsExecuted++
                        val status = content.v1.status
                        if (status == AppOperationStatus.COMPLETED) cmdsSucceeded++
                        else if (status == AppOperationStatus.FAILED) cmdsFailed++
                        content.v1.durationMs?.let { cmdDurationMs += it }
                        lastWasUser = false
                    }
                    is HydratedConversationItemContent.FileChange -> {
                        files += content.v1.changes.size
                        lastWasUser = false
                    }
                    is HydratedConversationItemContent.McpToolCall -> {
                        mcpCalls++
                        lastWasUser = false
                    }
                    is HydratedConversationItemContent.DynamicToolCall -> {
                        mcpCalls++
                        lastWasUser = false
                    }
                    is HydratedConversationItemContent.WebSearch -> {
                        webSearches++
                        lastWasUser = false
                    }
                    else -> {
                        lastWasUser = false
                    }
                }
            }

            return ConversationStatistics(
                totalMessages = userMessages + assistantMessages,
                userMessageCount = userMessages,
                assistantMessageCount = assistantMessages,
                turnCount = turns,
                commandsExecuted = cmdsExecuted,
                commandsSucceeded = cmdsSucceeded,
                commandsFailed = cmdsFailed,
                filesChanged = files,
                totalCommandDurationMs = cmdDurationMs,
                mcpToolCallCount = mcpCalls,
                webSearchCount = webSearches,
            )
        }
    }
}

data class ServerUsageData(
    val tokensByThread: List<Pair<String, Long>> = emptyList(),
    val activityByDay: List<Pair<LocalDate, Int>> = emptyList(),
    val modelUsage: List<Pair<String, Int>> = emptyList(),
    val rateLimits: uniffi.codex_mobile_client.RateLimitSnapshot? = null,
) {
    companion object {
        fun compute(
            threads: List<AppThreadSnapshot>,
            server: AppServerSnapshot,
        ): ServerUsageData {
            // Token usage by thread
            val tokensByThread = threads.mapNotNull { thread ->
                val tokens = thread.contextTokensUsed?.toLong() ?: return@mapNotNull null
                thread.displayTitle to tokens
            }.sortedByDescending { it.second }

            // Activity by day — use created_at/updated_at timestamps
            val activityMap = LinkedHashMap<LocalDate, Int>()
            for (thread in threads) {
                val ts = thread.info.updatedAt ?: thread.info.createdAt ?: continue
                val date = Instant.ofEpochSecond(ts).atZone(ZoneId.systemDefault()).toLocalDate()
                activityMap[date] = (activityMap[date] ?: 0) + 1
            }
            val activityByDay = activityMap.entries
                .map { it.key to it.value }
                .sortedBy { it.first }

            // Model usage breakdown
            val modelMap = LinkedHashMap<String, Int>()
            for (thread in threads) {
                val model = thread.model ?: thread.info.model ?: continue
                modelMap[model] = (modelMap[model] ?: 0) + 1
            }
            val modelUsage = modelMap.entries
                .map { it.key to it.value }
                .sortedByDescending { it.second }

            return ServerUsageData(
                tokensByThread = tokensByThread,
                activityByDay = activityByDay,
                modelUsage = modelUsage,
                rateLimits = server.rateLimits,
            )
        }
    }
}
