import Foundation
import Observation

/// History pages and live progress share one serialized reader. Playback never
/// depends on this model, and starting an import is never automatically retried.
@MainActor
@Observable
final class ImportsModel {
    static let pageSize = 20

    private(set) var jobs: [ImportJob] = []
    private(set) var error: String?
    private(set) var moreError: String?
    private(set) var isLoadingMore = false
    private(set) var loadTrigger = 0
    private var nextCursor: String?
    private var legacyJobs: [ImportJob] = []
    private var seen: [String: ImportJob] = [:]
    private var isRefreshing = false
    private var needsRefresh = true
    private var initialized = false
    private var generation = 0
    private var source: URL?
    private var pagination = false

    var canLoadMore: Bool {
        pagination ? nextCursor != nil : jobs.count < legacyJobs.count
    }

    func configure(client: VtampClient?, app: AppModel) {
        let supportsPages = app.server?.importsPagination == true
        guard source != client?.baseURL || pagination != supportsPages else { return }
        generation += 1
        source = client?.baseURL
        pagination = supportsPages
        jobs = []
        legacyJobs = []
        seen = [:]
        nextCursor = nil
        error = nil
        moreError = nil
        isRefreshing = false
        isLoadingMore = false
        needsRefresh = true
        initialized = false
        loadTrigger += 1
    }

    private func current(_ token: Int, client: VtampClient, app: AppModel) -> Bool {
        !Task.isCancelled && token == generation && source == client.baseURL && app.client?.baseURL == source
    }

    func poll(client: VtampClient, app: AppModel) async {
        var last = ContinuousClock.now - .seconds(60)
        while !Task.isCancelled {
            if needsRefresh || seen.values.contains(where: { !$0.isTerminal }) || ContinuousClock.now - last >= .seconds(10) {
                await refresh(client: client, app: app)
                last = .now
            }
            try? await Task.sleep(for: .seconds(1))
        }
    }

    func refresh(client: VtampClient, app: AppModel) async {
        guard app.client?.baseURL == client.baseURL, !Task.isCancelled else { return }
        configure(client: client, app: app)
        guard !isRefreshing, !isLoadingMore else { needsRefresh = true; return }
        let token = generation
        isRefreshing = true
        needsRefresh = false
        defer {
            if token == generation {
                isRefreshing = false
                loadTrigger += 1
            }
        }
        do {
            if pagination {
                var page = try await client.rpc(.importsPage(cursor: nil, limit: Self.pageSize), as: ImportsPage.self)
                guard current(token, client: client, app: app) else { return }
                var fresh = page.jobs
                var observedActive = page.activeJobs
                let oldIDs = Set(jobs.map(\.id))
                // A burst of new jobs can fill more than one page. Bridge to the
                // existing window before keeping its tail, so none are skipped.
                var visited: Set<String> = []
                while !oldIDs.isEmpty, !fresh.contains(where: { oldIDs.contains($0.id) }), let cursor = page.nextCursor {
                    guard visited.insert(cursor).inserted else { throw VtampError.malformed("Repeated imports cursor") }
                    page = try await client.rpc(.importsPage(cursor: cursor, limit: Self.pageSize), as: ImportsPage.self)
                    guard current(token, client: client, app: app) else { return }
                    fresh += page.jobs
                    observedActive += page.activeJobs
                }
                // History retention can remove rows in the cached tail. Once
                // growth exceeds the server's 100 finished + 32 live bound,
                // reconcile the full bounded window rather than keeping ghosts.
                if unique(fresh + jobs).count > 132 {
                    while let cursor = page.nextCursor {
                        guard visited.insert(cursor).inserted else { throw VtampError.malformed("Repeated imports cursor") }
                        page = try await client.rpc(.importsPage(cursor: cursor, limit: Self.pageSize), as: ImportsPage.self)
                        guard current(token, client: client, app: app) else { return }
                        fresh += page.jobs
                        observedActive += page.activeJobs
                    }
                }
                var updates = observedActive + fresh + page.activeJobs
                let reported = Set((fresh + page.activeJobs).map(\.id))
                var expired: Set<String> = []
                // A job can finish outside the history window. Resolve it once
                // instead of losing its final counts and Library notification.
                for before in unique(Array(seen.values) + observedActive) where !before.isTerminal && !reported.contains(before.id) {
                    do {
                        let status = try await client.rpc(.importStatus(id: before.id), as: ImportStatus.self)
                        guard current(token, client: client, app: app) else { return }
                        updates.append(status.job)
                    } catch let failure {
                        guard current(token, client: client, app: app) else { return }
                        if case let .server(remote) = failure, remote.code == "import_not_found" {
                            expired.insert(before.id)
                        } else {
                            throw failure
                        }
                    }
                }
                var tail: [ImportJob] = []
                var cursor = page.nextCursor
                if page.nextCursor != nil,
                   let overlap = fresh.last(where: { oldIDs.contains($0.id) }),
                   let index = jobs.firstIndex(where: { $0.id == overlap.id }), index + 1 < jobs.count {
                    tail = Array(jobs[(index + 1)...])
                    cursor = nextCursor
                }
                observe(updates, expired: expired, app: app)
                jobs = unique(fresh + tail).filter { !expired.contains($0.id) }.map { seen[$0.id] ?? $0 }
                nextCursor = cursor
            } else {
                let all = try await client.rpc(.imports, as: [ImportJob].self)
                guard current(token, client: client, app: app) else { return }
                // Preserve the oldest revealed row when new jobs arrive.
                let count = jobs.last.flatMap { last in all.firstIndex(where: { $0.id == last.id }).map { $0 + 1 } }
                    ?? max(Self.pageSize, jobs.count)
                observe(all, app: app)
                legacyJobs = all.map { seen[$0.id] ?? $0 }
                jobs = Array(legacyJobs.prefix(count))
            }
            initialized = true
            pruneSeen()
            error = nil
            app.reachable()
        } catch {
            guard current(token, client: client, app: app) else { return }
            self.error = error.localizedDescription
            if let failure = error as? VtampError { app.report(failure) }
        }
    }

    func loadMore(client: VtampClient, app: AppModel) async {
        guard app.client?.baseURL == client.baseURL, !Task.isCancelled else { return }
        configure(client: client, app: app)
        guard canLoadMore, !isLoadingMore, !isRefreshing else { return }
        if !pagination {
            jobs = Array(legacyJobs.prefix(jobs.count + Self.pageSize))
            loadTrigger += 1
            return
        }
        let token = generation
        let cursor = nextCursor
        isLoadingMore = true
        defer { if token == generation { isLoadingMore = false } }
        do {
            let page = try await client.rpc(.importsPage(cursor: cursor, limit: Self.pageSize), as: ImportsPage.self)
            guard current(token, client: client, app: app) else { return }
            guard page.nextCursor == nil || page.nextCursor != cursor else {
                throw VtampError.malformed("Repeated imports cursor")
            }
            // The next head refresh resolves active jobs missing from this page.
            observe(page.jobs + page.activeJobs, discoverNew: false, app: app)
            jobs = unique(jobs + page.jobs).map { seen[$0.id] ?? $0 }
            nextCursor = page.nextCursor
            pruneSeen()
            moreError = nil
            app.reachable()
            loadTrigger += 1
        } catch {
            guard current(token, client: client, app: app) else { return }
            moreError = error.localizedDescription
            if let failure = error as? VtampError { app.report(failure) }
        }
    }

    private func unique(_ jobs: [ImportJob]) -> [ImportJob] {
        var ids: Set<String> = []
        return jobs.filter { ids.insert($0.id).inserted }
    }

    private func observe(_ updates: [ImportJob], expired: Set<String> = [], discoverNew: Bool = true, app: AppModel) {
        var changed = !expired.isEmpty
        for job in updates {
            if let before = seen[job.id] {
                guard job.revision >= before.revision else { continue }
                if job.added + job.updated > before.added + before.updated || (job.isTerminal && !before.isTerminal) {
                    changed = true
                }
            } else if initialized && discoverNew && job.added + job.updated > 0 {
                changed = true
            }
            seen[job.id] = job
        }
        for id in expired { seen.removeValue(forKey: id) }
        if changed { app.libraryChanged() }
    }

    private func pruneSeen() {
        // Keep recently observed terminal revisions even when their rows have
        // not been revealed, so later page replies cannot revive an old job.
        let recent = seen.values.filter(\.isTerminal).sorted {
            if $0.startedAtMs != $1.startedAtMs { return $0.startedAtMs > $1.startedAtMs }
            return $0.id > $1.id
        }.prefix(100)
        let retained = Set((jobs + legacyJobs + recent).map(\.id))
        seen = seen.filter { retained.contains($0.key) || !$0.value.isTerminal }
    }

    func start(_ request: ImportRequest, client: VtampClient, app: AppModel) async throws(VtampError) {
        _ = try await client.rpc(.importStart(request), as: ImportStarted.self)
        needsRefresh = true
        await refresh(client: client, app: app)
    }

    func cancel(_ job: ImportJob, client: VtampClient, app: AppModel) async {
        let token = generation
        do {
            _ = try await client.rpc(.importCancel(id: job.id), as: ImportJob.self)
            guard current(token, client: client, app: app) else { return }
            await refresh(client: client, app: app)
        } catch {
            if current(token, client: client, app: app) { self.error = error.localizedDescription }
        }
    }

    func retry(_ job: ImportJob, client: VtampClient, app: AppModel) async {
        let token = generation
        do {
            _ = try await client.rpc(.importRetry(id: job.id), as: ImportStarted.self)
            guard current(token, client: client, app: app) else { return }
            needsRefresh = true
            await refresh(client: client, app: app)
        } catch {
            if current(token, client: client, app: app) { self.error = error.localizedDescription }
        }
    }
}
