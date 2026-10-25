# 0.2.0 release preparation

Prepare the landed frame-analysis work for a coordinated 0.2.0 release.
Publishing, tagging, and the next domain/tooling development cycle are outside
this pass.

1. Completed: reproduce and fix the missing capture-read status in pacing JSON, with
   end-to-end coverage for complete, truncated, and mixed-instance captures.
2. Completed: align workspace versions, internal requirements, current examples, migration
   guidance, and the analysis guide. Preserve historical capture versions.
3. Completed: run workspace checks and inspect all publishable package contents. Verify
   packages with their unpublished siblings staged locally, then build and run
   an external consumer against the packaged sources.
4. Completed: record the package order, rehearsal results, and remaining publication steps
   in the release documentation.

The main risks are accepting incomplete data without machine-readable warning,
resolving a package against an older Footfall dependency, and mistaking a staged
package rehearsal for proof that 0.2.0 is already available on crates.io.
