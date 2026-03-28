pub mod dashboard;
pub mod home;
pub mod setup;

// Only re-export what main.rs needs; tab components are used internally by DashboardPage
pub use dashboard::DashboardPage;
pub use home::CoachHome;
pub use setup::SetupPage;
