//! Autocomplete for the SQL console: a GtkSourceView completion provider
//! that asks `atlas_core::complete` for keywords, tables and columns.

use std::cell::RefCell;
use std::rc::Rc;

use atlas_core::complete::{self, Suggestion, SuggestionKind};
use gtk::glib;
use gtk::glib::subclass::prelude::*;
use gtk::prelude::*;
use sourceview5::prelude::*;
use sourceview5::subclass::prelude::*;

use super::Ctx;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Proposal {
        pub item: RefCell<Option<Suggestion>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Proposal {
        const NAME: &'static str = "AtlasCompletionProposal";
        type Type = super::Proposal;
        type Interfaces = (sourceview5::CompletionProposal,);
    }

    impl ObjectImpl for Proposal {}
    impl CompletionProposalImpl for Proposal {}

    #[derive(Default)]
    pub struct Provider {
        pub ctx: RefCell<Option<Rc<Ctx>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Provider {
        const NAME: &'static str = "AtlasCompletionProvider";
        type Type = super::Provider;
        type Interfaces = (sourceview5::CompletionProvider,);
    }

    impl ObjectImpl for Provider {}

    impl CompletionProviderImpl for Provider {
        fn title(&self) -> Option<glib::GString> {
            Some("Atlas".into())
        }

        fn is_trigger(&self, _iter: &gtk::TextIter, c: char) -> bool {
            c == '.'
        }

        fn populate(&self, context: &sourceview5::CompletionContext) -> Result<gio::ListModel, glib::Error> {
            let store = gio::ListStore::new::<super::Proposal>();
            let (Some(buffer), Some((_, end))) = (context.buffer(), context.bounds()) else {
                return Ok(store.upcast());
            };
            let (start, stop) = buffer.bounds();
            let all = buffer.text(&start, &stop, false).to_string();
            let before = buffer.text(&start, &end, false).to_string();
            let ctx = self.ctx.borrow();
            let schema = ctx.as_ref().and_then(|c| c.schema.borrow().clone());
            if let Some(schema) = schema {
                for s in complete::suggest(&all, &before, &schema).into_iter().take(200) {
                    let p: super::Proposal = glib::Object::new();
                    *p.imp().item.borrow_mut() = Some(s);
                    store.append(&p);
                }
            }
            Ok(store.upcast())
        }

        fn display(
            &self,
            _context: &sourceview5::CompletionContext,
            proposal: &sourceview5::CompletionProposal,
            cell: &sourceview5::CompletionCell,
        ) {
            let Some(p) = proposal.downcast_ref::<super::Proposal>() else { return };
            let item = p.imp().item.borrow();
            let Some(item) = item.as_ref() else { return };
            match cell.column() {
                sourceview5::CompletionColumn::TypedText => cell.set_text(Some(&item.text)),
                sourceview5::CompletionColumn::Comment => cell.set_text(Some(&item.detail)),
                sourceview5::CompletionColumn::Before => cell.set_text(Some(match item.kind {
                    SuggestionKind::Keyword => "",
                    SuggestionKind::Table => "T",
                    SuggestionKind::Column => "C",
                })),
                _ => cell.set_text(None),
            }
        }

        fn activate(&self, context: &sourceview5::CompletionContext, proposal: &sourceview5::CompletionProposal) {
            let Some(p) = proposal.downcast_ref::<super::Proposal>() else { return };
            let Some(text) = p.imp().item.borrow().as_ref().map(|i| i.text.clone()) else { return };
            let (Some(buffer), Some((mut start, mut end))) = (context.buffer(), context.bounds()) else { return };
            buffer.begin_user_action();
            buffer.delete(&mut start, &mut end);
            buffer.insert(&mut start, &text);
            buffer.end_user_action();
        }

        fn refilter(&self, context: &sourceview5::CompletionContext, model: &gio::ListModel) {
            // Asking again is cheap; the qualifier may have changed too.
            if let Ok(fresh) = self.populate(context) {
                let _ = model;
                context.set_proposals_for_provider(&*self.obj(), Some(&fresh));
            }
        }
    }
}

glib::wrapper! {
    pub struct Proposal(ObjectSubclass<imp::Proposal>) @implements sourceview5::CompletionProposal;
}

glib::wrapper! {
    pub struct Provider(ObjectSubclass<imp::Provider>) @implements sourceview5::CompletionProvider;
}

use gtk::gio;

/// Adds schema completion to `view`.
pub fn attach(view: &sourceview5::View, ctx: Rc<Ctx>) {
    let provider: Provider = glib::Object::new();
    *provider.imp().ctx.borrow_mut() = Some(ctx);
    let completion = view.completion();
    completion.add_provider(&provider);
    completion.set_show_icons(false);
    completion.set_select_on_show(true);
}
