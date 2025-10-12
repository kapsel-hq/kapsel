# Product Roadmap

This roadmap defines the phased delivery plan for kapsel, from MVP through enterprise features. Each phase builds upon the previous, with clear success criteria and deliverables.

## Timeline Overview

| Phase | Duration | Target Date | Primary Goal |
|---|---|---|---|
| Phase 1: MVP | 3 months | March 2024 | Core reliability engine |
| Phase 2: Beta | 3 months | June 2024 | Developer experience & launch |
| Phase 3: Stability | 1 month | July 2024 | Production hardening |
| Phase 4: Scale | Ongoing | August 2024+ | Enterprise features |

## Phase 1: MVP (Months 1-3)

**Goal**: Build and validate core value proposition with early adopters.

### Core Engine
- [x] PostgreSQL schema with idempotency enforcement
- [ ] HTTP receiver with Axum
- [ ] HMAC-SHA256 signature validation
- [ ] Bounded channel with backpressure
- [ ] Dispatcher with batch persistence
- [ ] Worker pool with `FOR UPDATE SKIP LOCKED`
- [ ] Exponential backoff retry logic
- [ ] Basic circuit breaker (per endpoint)
- [ ] Structured logging with correlation IDs
- [ ] Prometheus metrics endpoint

### User Experience
- [ ] Unique webhook URL generation
- [ ] Read-only event dashboard
- [ ] Event timeline with filtering
- [ ] Event detail view with attempts
- [ ] Manual retry button
- [ ] Account creation (email/password)
- [ ] Single tenant isolation

### Operations
- [ ] Docker container build
- [ ] Health check endpoints
- [ ] Graceful shutdown handling
- [ ] Basic rate limiting (fixed)
- [ ] Development environment setup
- [ ] Integration test suite

### Success Criteria
- 10 active beta users
- 99.9% delivery success rate
- < 10ms ingestion latency (p99)
- < 100ms first delivery attempt (p50)
- Zero data loss events

### Deliverables
- Working prototype deployed to staging
- Basic documentation (README, API examples)
- Feedback from 5+ beta users
- Performance baseline established

## Phase 2: Beta & Launch (Months 3-6)

**Goal**: Refine developer experience and launch publicly with billing.

### Technical Enhancements
- [ ] TigerBeetle integration for audit log
- [ ] Two-phase persistence model
- [ ] Reconciliation worker
- [ ] Advanced retry policies (configurable)
- [ ] Per-endpoint configuration
- [ ] Idempotency strategies (header/content/source)
- [ ] Connection pooling optimization
- [ ] Batch operation improvements

### Developer Experience
- [ ] Public REST API v1
  - [ ] Endpoint management (CRUD)
  - [ ] Event queries with pagination
  - [ ] Manual retry trigger
  - [ ] Bulk operations
- [ ] API authentication (Bearer tokens)
- [ ] OpenAPI specification
- [ ] SDK generation (TypeScript, Python)
- [ ] Comprehensive documentation
  - [ ] Getting started guide
  - [ ] API reference
  - [ ] Integration examples
  - [ ] Webhook provider guides

### Commercialization
- [ ] Stripe billing integration
- [ ] Subscription tiers
  - Free: 10K events/month
  - Starter: 100K events/month ($29)
  - Growth: 1M events/month ($99)
  - Scale: 10M events/month ($299)
- [ ] Usage metering and limits
- [ ] Billing portal
- [ ] Invoice generation

### Operations
- [ ] Multi-tenant isolation
- [ ] Dynamic rate limiting per tier
- [ ] Data retention policies
- [ ] Automated backup/restore
- [ ] Monitoring and alerting
- [ ] On-call rotation setup

### Success Criteria
- 100 active users
- $1000 MRR
- 99.95% uptime
- < 5 minute incident response
- NPS > 50

### Deliverables
- Production deployment on AWS/GCP
- Full API documentation
- 5+ integration guides
- Billing and subscription management
- Status page

## Phase 3: Stability Soak (Months 6-7)

**Goal**: Prove reliability at scale through comprehensive testing.

### Reliability Testing
- [ ] Deterministic chaos testing framework
  - [ ] Network partition simulation
  - [ ] Database failure injection
  - [ ] Clock skew testing
  - [ ] Resource exhaustion scenarios
- [ ] Load testing (10K webhooks/sec)
- [ ] Stress testing (find breaking point)
- [ ] Soak testing (24 hour run)
- [ ] Chaos experiments in staging
  - [ ] Random pod kills
  - [ ] Network latency injection
  - [ ] Database connection drops

### Hardening
- [ ] Circuit breaker refinements
- [ ] Adaptive retry backoff
- [ ] Connection pool tuning
- [ ] Query optimization
- [ ] Memory leak detection
- [ ] Graceful degradation paths

### Observability
- [ ] Distributed tracing (OpenTelemetry)
- [ ] Custom dashboards (Grafana)
- [ ] SLO definition and tracking
- [ ] Synthetic monitoring
- [ ] Alert refinement
- [ ] Runbook automation

### Success Criteria
- Zero data loss under failure
- < 30s recovery from any failure
- 10K webhooks/sec sustained
- < 2GB memory at peak load
- All chaos tests passing

### Deliverables
- Chaos test suite
- Load test reports
- Performance optimization guide
- Incident response playbooks
- SLO dashboard

## Phase 4: Scale & Enterprise (Months 7+)

**Goal**: Expand to enterprise features and advanced capabilities.

### Advanced Features
- [ ] Webhook transformations
  - [ ] JSONPath filtering
  - [ ] Header manipulation
  - [ ] Payload transformation
- [ ] Event replay capabilities
  - [ ] Time-range replay
  - [ ] Bulk replay with deduplication
  - [ ] Replay scheduling
- [ ] Advanced routing
  - [ ] Multiple destinations
  - [ ] Conditional routing
  - [ ] Fan-out patterns
- [ ] Webhook sources
  - [ ] Pull from SQS/Kafka
  - [ ] Scheduled polling
  - [ ] GraphQL subscriptions

### Security & Compliance
- [ ] Signed attestations API
  - [ ] JWT-signed receipts
  - [ ] Cryptographic proof of delivery
  - [ ] Chain of custody
- [ ] mTLS support
- [ ] IP allowlisting
- [ ] RBAC implementation
- [ ] Audit log export
- [ ] SOC 2 Type II certification
- [ ] GDPR compliance tools

### Enterprise Features
- [ ] Team management
  - [ ] User invites
  - [ ] Role management
  - [ ] Permission models
- [ ] SSO integration (SAML/OIDC)
- [ ] Custom contracts
- [ ] SLA guarantees
- [ ] Dedicated infrastructure
- [ ] White-label options

### Platform Expansion
- [ ] Multi-region deployment
- [ ] Edge ingestion points
- [ ] CDN integration
- [ ] Global load balancing
- [ ] Data residency options

### Deliverables
- Enterprise onboarding process
- Compliance documentation
- Advanced features documentation
- Migration tools
- Partner integration program

## Future Considerations

### Potential Features (Not Committed)
- GraphQL API alongside REST
- Webhook debugging/replay UI
- Webhook marketplace/templates
- Built-in transformations library
- Native integrations (Zapier, etc.)
- Webhook analytics dashboard
- Anomaly detection
- Auto-scaling policies

### Technical Debt Items
- PostgreSQL partitioning for scale
- TigerBeetle cluster management
- Zero-downtime migrations
- Multi-tenant resource isolation
- Cost optimization pass
- Performance profiling

## Success Metrics

### Technical KPIs
- Ingestion latency (p50, p99)
- Delivery success rate
- System availability
- Time to recovery
- Queue depth
- Resource utilization

### Business KPIs
- Monthly active users
- Monthly recurring revenue
- Customer acquisition cost
- Churn rate
- Net promoter score
- Support ticket volume

### Product KPIs
- API adoption rate
- Feature usage metrics
- Time to first webhook
- Integration completion rate
- Dashboard engagement

## Risk Mitigation

| Risk | Impact | Mitigation |
|---|---|---|
| Scalability limits | High | Early load testing, horizontal scaling design |
| Security breach | Critical | Security-first design, regular audits |
| Data loss | Critical | Two-phase persistence, comprehensive backup |
| Market fit | High | Continuous customer feedback, rapid iteration |
| Technical debt | Medium | Regular refactoring sprints, code review |
| Operational complexity | Medium | Automation, runbooks, monitoring |

## Release Process

### Version Strategy
- Semantic versioning for API
- Rolling releases for service
- Feature flags for gradual rollout
- Beta channel for early adopters

### Deployment Cadence
- Weekly releases to staging
- Bi-weekly production deploys
- Hotfixes as needed
- Major versions quarterly

### Communication
- Changelog maintenance
- Status page updates
- Email notifications for changes
- Developer blog posts
