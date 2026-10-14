use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex};
use std::time::Instant;

use crate::config::{Bundle, GitSource, Source};
use crate::fetcher;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Remote {
    Git(GitSource),
    Url(String),
}

#[derive(Debug)]
pub enum Progress {
    Assembling {
        bundle: String,
    },
    Linking {
        bundle: String,
    },
    Plan {
        bundles: usize,
        remote_sources: usize,
        workers: usize,
    },
    Started {
        label: String,
        kind: &'static str,
    },
    Finished {
        label: String,
        seconds: f64,
        success: bool,
    },
}

pub struct Prepared {
    sources: BTreeMap<Remote, Result<PathBuf, String>>,
}

impl Prepared {
    pub fn git(&self, source: &GitSource) -> anyhow::Result<PathBuf> {
        self.get(&Remote::Git(source.clone()))
    }

    pub fn url(&self, url: &str) -> anyhow::Result<PathBuf> {
        self.get(&Remote::Url(url.into()))
    }

    fn get(&self, source: &Remote) -> anyhow::Result<PathBuf> {
        match self.sources.get(source) {
            Some(Ok(path)) => Ok(path.clone()),
            Some(Err(error)) => anyhow::bail!("{error}"),
            None => anyhow::bail!("remote source was not prepared"),
        }
    }
}

pub fn prepare(
    bundles: &HashMap<String, Bundle>,
    cache: &Path,
    workers: usize,
    progress: &mut impl FnMut(Progress),
) -> Prepared {
    let mut jobs = BTreeMap::<Remote, String>::new();
    let mut names: Vec<_> = bundles.keys().collect();
    names.sort();
    for name in names {
        let bundle = &bundles[name];
        let mut skills: Vec<_> = bundle.skills.iter().collect();
        skills.sort_by_key(|(name, _)| *name);
        let specs = std::iter::once((name.clone(), &bundle.source)).chain(
            skills
                .into_iter()
                .map(|(skill, spec)| (format!("{name}/{skill}"), spec)),
        );
        for (label, spec) in specs {
            let remote = match spec.resolve() {
                Ok(Some(Source::Git { repo, git_ref, .. })) => {
                    Remote::Git(GitSource::new(&repo, git_ref.as_deref()))
                }
                Ok(Some(Source::Url(url))) => Remote::Url(url),
                _ => continue,
            };
            jobs.entry(remote).or_insert(label);
        }
    }
    let workers = workers.clamp(1, 16).min(jobs.len());
    progress(Progress::Plan {
        bundles: bundles.len(),
        remote_sources: jobs.len(),
        workers,
    });
    let queue = Mutex::new(VecDeque::from_iter(jobs));
    let (sender, receiver) = mpsc::channel();
    let mut sources = BTreeMap::new();
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let sender = sender.clone();
            let queue = &queue;
            scope.spawn(move || loop {
                let Some((source, label)) = queue.lock().expect("fetch queue lock").pop_front()
                else {
                    break;
                };
                let kind = match &source {
                    Remote::Git(_) => "git",
                    Remote::Url(_) => "url",
                };
                let _ = sender.send((
                    None,
                    Some(Progress::Started {
                        label: label.clone(),
                        kind,
                    }),
                ));
                let started = Instant::now();
                let result = match &source {
                    Remote::Git(source) => fetcher::fetch_git_repo(source, cache),
                    Remote::Url(url) => fetcher::cache_url(url, cache),
                }
                .map_err(|error| format!("{error:#}"));
                let event = Progress::Finished {
                    label,
                    seconds: started.elapsed().as_secs_f64(),
                    success: result.is_ok(),
                };
                let _ = sender.send((Some((source, result)), Some(event)));
            });
        }
        drop(sender);
        for (result, event) in receiver {
            if let Some(event) = event {
                progress(event);
            }
            if let Some((source, result)) = result {
                sources.insert(source, result);
            }
        }
    });
    Prepared { sources }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Condvar};
    use std::time::Duration;

    #[test]
    fn distinct_downloads_overlap_and_duplicate_urls_fetch_once() {
        let temp = tempfile::tempdir().unwrap();
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr();
        let gate = Arc::new((Mutex::new(0usize), Condvar::new()));
        let handler = std::thread::spawn(move || {
            let mut handlers = Vec::new();
            for _ in 0..2 {
                let request = server
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap()
                    .expect("both unique URLs requested");
                let gate = gate.clone();
                handlers.push(std::thread::spawn(move || {
                    let (mutex, condvar) = &*gate;
                    let mut active = mutex.lock().unwrap();
                    *active += 1;
                    condvar.notify_all();
                    let (active, _) = condvar
                        .wait_timeout_while(active, Duration::from_secs(2), |active| *active < 2)
                        .unwrap();
                    assert_eq!(*active, 2, "downloads must overlap");
                    drop(active);
                    request
                        .respond(tiny_http::Response::from_string(
                            "---\nname: fixture\n---\n",
                        ))
                        .unwrap();
                }));
            }
            for thread in handlers {
                thread.join().unwrap();
            }
        });
        let config: crate::config::Config = toml::from_str(&format!(
            r#"
[bundles.test]
harnesses = ["pi"]
[bundles.test.skills.one]
url = "http://{address}/one"
[bundles.test.skills.duplicate]
url = "http://{address}/one"
[bundles.test.skills.two]
url = "http://{address}/two"
"#
        ))
        .unwrap();
        let prepared = prepare(&config.bundles, temp.path(), 2, &mut |_| {});
        handler.join().unwrap();
        assert_eq!(prepared.sources.len(), 2);
        assert!(prepared.sources.values().all(Result::is_ok));
    }
}
