use anyhow::{Context, Result};
use chrono::{Days, Local, TimeZone};
use headless_chrome::{Browser, browser::tab::Tab};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    thread,
    time::Duration,
};

const SEARCH_URL: &str =
    "https://www.104.com.tw/jobs/search/?jobsource=index_s&keyword=Rust&mode=s&order=16";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChromeVersion {
    web_socket_debugger_url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
struct JobListing {
    external_id: String,
    title: String,
    company: String,
    location: Option<String>,
    salary: Option<String>,
    description: Option<String>,
    url: String,
    published_at: Option<String>,
}

fn parse_job_list(value: &str) -> Result<Vec<JobListing>> {
    serde_json::from_str(value).context("failed to parse extracted 104 job list")
}

fn extract_job_list(tab: &Tab) -> Result<Vec<JobListing>> {
    let result = tab.evaluate(
        r#"(async () => {
            const jobs = new Map();
            const text = (root, selector) => {
                const value = root.querySelector(selector)?.innerText?.trim();
                return value || null;
            };
            const jobUrl = (root) => {
                const href = root.querySelector('a.info-job')?.href;
                if (!href) return null;
                try { return new URL(href).searchParams.get('url') || href; }
                catch (_) { return href; }
            };
            const collect = () => {
                for (const card of document.querySelectorAll('div[data-job-no]')) {
                    const externalId = card.getAttribute('data-job-no');
                    const url = jobUrl(card);
                    if (!externalId || !url) continue;
                    jobs.set(externalId, {
                        external_id: externalId,
                        title: text(card, '.info-name') || '',
                        company: text(card, '.info-company > a') || '',
                        location: text(card, '[data-gtm-joblist^="職缺-地區"]'),
                        salary: text(card, '[data-gtm-joblist^="職缺-薪資"]'),
                        description: text(card, '.info-content'),
                        url,
                        published_at: text(card, '.job-mobile__date'),
                    });
                }
            };
            const scroller = document.querySelector('.vue-recycle-scroller');
            for (let attempt = 0; attempt < 120; attempt += 1) {
                collect();
                if (!scroller) break;
                const previous = scroller.scrollTop;
                scroller.scrollTop += Math.max(window.innerHeight, 600);
                scroller.dispatchEvent(new Event('scroll', { bubbles: true }));
                await new Promise((resolve) => setTimeout(resolve, 100));
                if (scroller.scrollTop === previous) { collect(); break; }
            }
            collect();
            return JSON.stringify([...jobs.values()]);
        })()"#,
        true,
    )?;
    let value = result
        .value
        .context("104 job extraction returned no value")?;
    let json = value
        .as_str()
        .context("104 job extraction returned a non-string value")?;
    parse_job_list(json)
}

fn extract_total_pages(tab: &Tab) -> Result<usize> {
    let result = tab.evaluate(
        r#"Math.max(1, ...Array.from(document.querySelectorAll('a[href*="page="]'))
            .map((link) => Number(new URL(link.href).searchParams.get('page')))
            .filter((page) => Number.isInteger(page) && page > 0))"#,
        false,
    )?;
    let pages = result
        .value
        .context("104 pagination returned no value")?
        .as_u64()
        .context("104 pagination returned a non-integer value")?;
    usize::try_from(pages).context("104 page count does not fit in usize")
}

fn ensure_schema(connection: &Connection) -> Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS jobs (
            source TEXT NOT NULL, external_id TEXT NOT NULL, title TEXT NOT NULL,
            company TEXT NOT NULL, location TEXT, salary TEXT, description TEXT,
            url TEXT NOT NULL, published_at TEXT,
            first_seen_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            last_seen_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (source, external_id)
        );",
        )
        .context("failed to initialize SQLite schema")?;
    Ok(())
}

fn load_known_jobs(connection: &Connection) -> Result<HashMap<String, JobListing>> {
    let mut statement = connection
        .prepare(
            "SELECT external_id, title, company, location, salary, description, url, published_at
         FROM jobs WHERE source = '104'",
        )
        .context("failed to prepare known 104 jobs query")?;
    let rows = statement
        .query_map([], |row| {
            let job = JobListing {
                external_id: row.get(0)?,
                title: row.get(1)?,
                company: row.get(2)?,
                location: row.get(3)?,
                salary: row.get(4)?,
                description: row.get(5)?,
                url: row.get(6)?,
                published_at: row.get(7)?,
            };
            Ok((job.external_id.clone(), job))
        })
        .context("failed to query known 104 jobs")?;
    rows.collect::<rusqlite::Result<HashMap<_, _>>>()
        .context("failed to read known 104 jobs")
}

fn persist_jobs(connection: &mut Connection, jobs: &[JobListing]) -> Result<usize> {
    ensure_schema(connection)?;
    let transaction = connection
        .transaction()
        .context("failed to begin SQLite transaction")?;
    let mut statement = transaction
        .prepare(
            "INSERT INTO jobs (source, external_id, title, company, location, salary,
                           description, url, published_at)
         VALUES ('104', ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (source, external_id) DO UPDATE SET
           title = excluded.title, company = excluded.company, location = excluded.location,
           salary = excluded.salary, description = excluded.description, url = excluded.url,
           published_at = excluded.published_at, last_seen_at = CURRENT_TIMESTAMP",
        )
        .context("failed to prepare SQLite job upsert")?;
    for job in jobs {
        statement
            .execute(params![
                job.external_id,
                job.title,
                job.company,
                job.location,
                job.salary,
                job.description,
                job.url,
                job.published_at
            ])
            .with_context(|| format!("failed to persist 104 job {}", job.external_id))?;
    }
    drop(statement);
    transaction
        .commit()
        .context("failed to commit SQLite job upsert")?;
    Ok(jobs.len())
}

fn next_scheduled_run() -> Result<Duration> {
    let now = Local::now();
    let today = now.date_naive();
    for hour in [7, 17] {
        let candidate = today
            .and_hms_opt(hour, 0, 0)
            .and_then(|time| Local.from_local_datetime(&time).single());
        if let Some(candidate) = candidate.filter(|candidate| *candidate > now) {
            return (candidate - now)
                .to_std()
                .context("invalid scheduled run duration");
        }
    }
    let tomorrow = today
        .checked_add_days(Days::new(1))
        .context("failed to calculate tomorrow")?;
    let candidate = tomorrow
        .and_hms_opt(7, 0, 0)
        .and_then(|time| Local.from_local_datetime(&time).single())
        .context("failed to calculate tomorrow's 07:00 run")?;
    (candidate - now)
        .to_std()
        .context("invalid scheduled run duration")
}

fn run_check() -> Result<()> {
    let version: ChromeVersion = reqwest::blocking::get("http://127.0.0.1:9222/json/version")?
        .error_for_status()?
        .json()?;
    let browser = Browser::connect(version.web_socket_debugger_url)
        .context("failed to connect to Chromium")?;
    let tab = browser.new_tab().context("failed to create Chromium tab")?;
    tab.navigate_to(SEARCH_URL)
        .context("failed to navigate to the 104 Rust search")?
        .wait_until_navigated()
        .context("104 search page did not finish navigating")?;
    println!("Title: {:?}", tab.get_title()?);
    println!("URL: {}", tab.get_url());
    let html = tab.get_content().context("failed to get rendered HTML")?;
    println!("HTML size: {}", html.len());
    println!("Contains Rust: {}", html.contains("Rust"));
    std::fs::write("job-list.html", html).context("failed to save job list")?;
    println!("Saved rendered job list to job-list.html");
    thread::sleep(Duration::from_secs(2));

    let total_pages = extract_total_pages(&tab)?;
    let mut connection = Connection::open("jobs.sqlite3").context("failed to open jobs.sqlite3")?;
    ensure_schema(&connection)?;
    let known_jobs = load_known_jobs(&connection)?;
    let incremental_search = !known_jobs.is_empty();
    let mut seen_job_ids = HashSet::new();
    let mut jobs = Vec::new();
    let mut stopped_on_known_job = false;

    for page in 1..=total_pages {
        if page > 1 {
            tab.navigate_to(&format!("{SEARCH_URL}&page={page}"))
                .with_context(|| format!("failed to navigate to 104 page {page}"))?
                .wait_until_navigated()
                .with_context(|| format!("104 page {page} did not finish navigating"))?;
            thread::sleep(Duration::from_secs(2));
        }
        let page_jobs = extract_job_list(&tab)
            .with_context(|| format!("failed to extract rendered 104 page {page}"))?;
        for job in page_jobs {
            if let Some(previous_job) = known_jobs.get(&job.external_id) {
                if previous_job != &job {
                    println!("[UPDATE] {} {}", job.external_id, job.title);
                    jobs.push(job.clone());
                }
                if incremental_search {
                    stopped_on_known_job = true;
                    break;
                }
                continue;
            }
            if seen_job_ids.insert(job.external_id.clone()) {
                println!("[CREATE] {} {}", job.external_id, job.title);
                jobs.push(job);
            }
        }
        println!("Collected page {page}/{total_pages}: {} jobs", jobs.len());
        if stopped_on_known_job {
            println!("Stopped after reaching an existing job on page {page}");
            break;
        }
    }
    std::fs::write("job-list.json", serde_json::to_vec_pretty(&jobs)?)
        .context("failed to save extracted job list")?;
    println!(
        "Saved {} unique jobs from {total_pages} pages to job-list.json",
        jobs.len()
    );
    let persisted = persist_jobs(&mut connection, &jobs)?;
    println!("Persisted {persisted} jobs to jobs.sqlite3");
    Ok(())
}

pub fn run_service() -> Result<()> {
    println!("Running initial 104 job check");
    run_check().context("initial 104 job check failed")?;
    loop {
        let delay = next_scheduled_run()?;
        println!("Next 104 job check in {} seconds", delay.as_secs());
        thread::sleep(delay);
        if let Err(error) = run_check() {
            eprintln!("Scheduled 104 job check failed: {error:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{JobListing, parse_job_list, persist_jobs};
    use rusqlite::Connection;

    #[test]
    fn parses_the_rendered_104_job_shape() {
        let jobs = parse_job_list(
            r#"[{"external_id":"13191931","title":"Rust engineer","company":"Company",
                "location":"Taipei","salary":"面議","description":null,
                "url":"https://www.104.com.tw/job/7uqyj","published_at":"8/03"}]"#,
        )
        .expect("fixture should parse");

        assert_eq!(jobs[0].external_id, "13191931");
        assert_eq!(jobs[0].published_at.as_deref(), Some("8/03"));
    }

    #[test]
    fn persists_and_updates_jobs_in_sqlite() {
        let mut connection = Connection::open_in_memory().expect("in-memory database");
        let mut job = JobListing {
            external_id: "13191931".to_owned(),
            title: "old title".to_owned(),
            company: "company".to_owned(),
            location: None,
            salary: None,
            description: None,
            url: "https://www.104.com.tw/job/7uqyj".to_owned(),
            published_at: None,
        };

        persist_jobs(&mut connection, &[job.clone()]).expect("initial insert");
        job.title = "updated title".to_owned();
        persist_jobs(&mut connection, &[job]).expect("upsert");

        let (count, title): (i64, String) = connection
            .query_row(
                "SELECT COUNT(*), title FROM jobs WHERE source = '104' AND external_id = '13191931'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read persisted job");
        assert_eq!(count, 1);
        assert_eq!(title, "updated title");
    }
}
